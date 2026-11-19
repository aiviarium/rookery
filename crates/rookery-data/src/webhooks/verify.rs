//! Signature schemes. Each returns what replays are recognised by: a hash of the scheme and of what the
//! signature covers, never of anything the sender may change without the secret. Where the signature
//! covers a timestamp, that is the timestamp and the body (or the signed delivery id of the `standard`
//! scheme): a replay inside the tolerance has the same key, one outside it is refused as too old. Where it
//! covers the body alone (GitHub, `hmac` without a timestamp header), it is the body, which the server
//! refuses again for as long as it keeps keys ([`REPLAY_KEPT_SECS`]): a provider's unsigned delivery id
//! does not tell two deliveries apart. `none` signs nothing and has no key.

use axum::http::HeaderMap;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, KeyInit, Mac};
use rookery_proto::schedules::{WebhookScheme, WebhookVerify};

const DEFAULT_TOLERANCE: i64 = 300;
/// How long the keys of accepted deliveries are kept, and a body that a signature without a timestamp
/// covers is refused again: a week.
pub const REPLAY_KEPT_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Verified {
    /// What a replay of this delivery would share with it: a hash of what its signature covers.
    pub replay_key: Option<String>,
    /// The provider's id of the delivery, when it sends one. Only told to the job: unless the
    /// signature covers it, anyone may change it.
    pub delivery_id: Option<String>,
}

/// The replay key of a delivery whose signature covers `parts`.
fn replay_key(scheme: &str, parts: &[&[u8]]) -> Option<String> {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(scheme.as_bytes());
    for p in parts {
        // Each part with its length, so that two splits of the same bytes differ.
        h.update((p.len() as u64).to_be_bytes());
        h.update(p);
    }
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[derive(Clone, Copy)]
enum Alg {
    Sha1,
    Sha256,
    Sha512,
}

fn alg(name: Option<&str>) -> Result<Alg, String> {
    match name.unwrap_or("sha256") {
        "sha1" => Ok(Alg::Sha1),
        "sha256" => Ok(Alg::Sha256),
        "sha512" => Ok(Alg::Sha512),
        other => Err(format!("unknown algorithm {other}")),
    }
}

/// Whether `sig` is the HMAC of the concatenated `parts` under `key`, in constant time.
fn matches(alg: Alg, key: &[u8], parts: &[&[u8]], sig: &[u8]) -> bool {
    macro_rules! check {
        ($h:ty) => {{
            let Ok(mut m) = <Hmac<$h> as KeyInit>::new_from_slice(key) else {
                return false;
            };
            for p in parts {
                m.update(p);
            }
            m.verify_slice(sig).is_ok()
        }};
    }
    match alg {
        Alg::Sha1 => check!(sha1::Sha1),
        Alg::Sha256 => check!(sha2::Sha256),
        Alg::Sha512 => check!(sha2::Sha512),
    }
}

/// The HMAC of `parts`, for tests and senders.
pub fn sign(algorithm: &str, key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    macro_rules! sign {
        ($h:ty) => {{
            let mut m = <Hmac<$h> as KeyInit>::new_from_slice(key).expect("any key length");
            for p in parts {
                m.update(p);
            }
            m.finalize().into_bytes().to_vec()
        }};
    }
    match algorithm {
        "sha1" => sign!(sha1::Sha1),
        "sha512" => sign!(sha2::Sha512),
        _ => sign!(sha2::Sha256),
    }
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok()).map(str::trim)
}

fn need<'a>(h: &'a HeaderMap, name: &str) -> Result<&'a str, String> {
    header(h, name).ok_or_else(|| format!("the {name} header is missing"))
}

fn fresh(ts: &str, now: i64, tolerance: i64) -> Result<(), String> {
    let t: i64 = ts
        .parse()
        .map_err(|_| format!("the timestamp {ts:?} is not a number"))?;
    if (now - t).abs() > tolerance {
        return Err(format!(
            "the timestamp is more than {tolerance} s away from now; a replay or a wrong clock"
        ));
    }
    Ok(())
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    hex_decode(s.trim())
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

const BAD: &str = "the signature does not match";

/// Verifies a delivery. `secret` is `None` only for the `none` scheme.
pub fn verify(
    v: &WebhookVerify,
    secret: Option<&[u8]>,
    headers: &HeaderMap,
    body: &[u8],
    now: i64,
) -> Result<Verified, String> {
    let tolerance = v.tolerance_secs.map(i64::from).unwrap_or(DEFAULT_TOLERANCE);
    let key = || secret.ok_or_else(|| "the webhook's secret is not set".to_string());
    match v.scheme {
        WebhookScheme::None => Ok(Verified::default()),
        WebhookScheme::Github => {
            let sig = need(headers, "x-hub-signature-256")?;
            let hex = sig
                .strip_prefix("sha256=")
                .ok_or("the signature is not sha256=<hex>")?;
            let sig = unhex(hex).ok_or("the signature is not hex")?;
            if !matches(Alg::Sha256, key()?, &[body], &sig) {
                return Err(BAD.into());
            }
            Ok(Verified {
                replay_key: replay_key("github", &[body]),
                delivery_id: header(headers, "x-github-delivery").map(str::to_string),
            })
        }
        WebhookScheme::Stripe => {
            let h = need(headers, "stripe-signature")?;
            let mut ts = None;
            let mut sigs = Vec::new();
            for part in h.split(',') {
                match part.trim().split_once('=') {
                    Some(("t", t)) => ts = Some(t.to_string()),
                    Some(("v1", s)) => sigs.push(s.to_string()),
                    _ => {}
                }
            }
            let ts = ts.ok_or("the signature has no timestamp")?;
            fresh(&ts, now, tolerance)?;
            let key = key()?;
            let hit = sigs.iter().find(|s| {
                unhex(s)
                    .is_some_and(|b| matches(Alg::Sha256, key, &[ts.as_bytes(), b".", body], &b))
            });
            match hit {
                Some(_) => Ok(Verified {
                    replay_key: replay_key("stripe", &[ts.as_bytes(), body]),
                    delivery_id: None,
                }),
                None => Err(BAD.into()),
            }
        }
        WebhookScheme::Slack => {
            let ts = need(headers, "x-slack-request-timestamp")?;
            fresh(ts, now, tolerance)?;
            let sig = need(headers, "x-slack-signature")?;
            let hex = sig
                .strip_prefix("v0=")
                .ok_or("the signature is not v0=<hex>")?;
            let b = unhex(hex).ok_or("the signature is not hex")?;
            if !matches(
                Alg::Sha256,
                key()?,
                &[b"v0:", ts.as_bytes(), b":", body],
                &b,
            ) {
                return Err(BAD.into());
            }
            Ok(Verified {
                replay_key: replay_key("slack", &[ts.as_bytes(), body]),
                delivery_id: None,
            })
        }
        WebhookScheme::Standard => {
            let id = need(headers, "webhook-id")?;
            let ts = need(headers, "webhook-timestamp")?;
            fresh(ts, now, tolerance)?;
            let sigs = need(headers, "webhook-signature")?;
            let raw = key()?;
            // `whsec_<base64>` secrets are the base64 of the key.
            let decoded = std::str::from_utf8(raw)
                .ok()
                .and_then(|s| s.strip_prefix("whsec_"))
                .and_then(|s| B64.decode(s).ok());
            let key = decoded.as_deref().unwrap_or(raw);
            let ok = sigs.split(' ').any(|s| {
                s.strip_prefix("v1,")
                    .and_then(|b| B64.decode(b).ok())
                    .is_some_and(|b| {
                        matches(
                            Alg::Sha256,
                            key,
                            &[id.as_bytes(), b".", ts.as_bytes(), b".", body],
                            &b,
                        )
                    })
            });
            if !ok {
                return Err(BAD.into());
            }
            // The id is signed: a provider's retry of a message, with a new timestamp, is the same key.
            Ok(Verified {
                replay_key: replay_key("standard", &[id.as_bytes()]),
                delivery_id: Some(id.to_string()),
            })
        }
        WebhookScheme::Hmac => {
            let alg = alg(v.algorithm.as_deref())?;
            let name = v
                .header
                .as_deref()
                .unwrap_or("x-signature")
                .to_ascii_lowercase();
            let mut sig = need(headers, &name)?;
            if let Some(p) = &v.prefix {
                sig = sig
                    .strip_prefix(p.as_str())
                    .ok_or_else(|| format!("the signature does not start with {p}"))?;
            }
            let b = match v.encoding.as_deref().unwrap_or("hex") {
                "base64" => B64.decode(sig).ok(),
                _ => unhex(sig),
            }
            .ok_or("the signature is not encoded as declared")?;
            let ts = match &v.timestamp_header {
                Some(h) => {
                    let t = need(headers, &h.to_ascii_lowercase())?;
                    fresh(t, now, tolerance)?;
                    Some(t)
                }
                None => None,
            };
            let ok = match ts {
                Some(t) => matches(alg, key()?, &[t.as_bytes(), b".", body], &b),
                None => matches(alg, key()?, &[body], &b),
            };
            if !ok {
                return Err(BAD.into());
            }
            let id = v
                .delivery_header
                .as_deref()
                .and_then(|h| header(headers, &h.to_ascii_lowercase()))
                .map(str::to_string);
            let replay_key = match ts {
                Some(t) => replay_key("hmac", &[t.as_bytes(), body]),
                None => replay_key("hmac", &[body]),
            };
            Ok(Verified {
                replay_key,
                delivery_id: id,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn headers(pairs: &[(&'static str, String)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn decl(json: serde_json::Value) -> WebhookVerify {
        serde_json::from_value(json).unwrap()
    }

    const KEY: &[u8] = b"It's a Secret to Everybody";
    const NOW: i64 = 1_800_000_000;

    #[test]
    fn github() {
        // GitHub's documented example: "Hello, World!" with this secret.
        let v = decl(serde_json::json!({ "scheme": "github", "secret": { "secret": "s" } }));
        let body = b"Hello, World!";
        let sig = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        let h = headers(&[
            ("x-hub-signature-256", sig.into()),
            ("x-github-delivery", "d-1".into()),
        ]);
        let ok = verify(&v, Some(KEY), &h, body, NOW).unwrap();
        assert_eq!(ok.replay_key, replay_key("github", &[body]));
        assert_eq!(ok.delivery_id.as_deref(), Some("d-1"));
        // The delivery id is not signed: another one is the same delivery.
        let other = headers(&[
            (
                "x-hub-signature-256",
                sig.to_uppercase().replace("SHA256=", "sha256="),
            ),
            ("x-github-delivery", "d-2".into()),
        ]);
        assert_eq!(
            verify(&v, Some(KEY), &other, body, NOW).unwrap().replay_key,
            ok.replay_key
        );
        assert_eq!(verify(&v, Some(b"wrong"), &h, body, NOW).unwrap_err(), BAD);
        assert!(verify(&v, Some(KEY), &h, b"Hello, World?", NOW).is_err());
        assert!(
            verify(&v, Some(KEY), &HeaderMap::new(), body, NOW)
                .unwrap_err()
                .contains("missing")
        );
    }

    #[test]
    fn stripe_slack_and_standard_check_the_timestamp() {
        let body = br#"{"id":"evt_1"}"#;
        let t = NOW.to_string();
        let s = hex(&sign("sha256", KEY, &[t.as_bytes(), b".", body]));
        let v = decl(serde_json::json!({ "scheme": "stripe", "secret": { "secret": "s" } }));
        let h = headers(&[("stripe-signature", format!("t={t},v1=deadbeef,v1={s}"))]);
        let key = verify(&v, Some(KEY), &h, body, NOW).unwrap().replay_key;
        assert_eq!(key, replay_key("stripe", &[t.as_bytes(), body]));
        // The same signature in upper case is the same delivery.
        let upper = headers(&[("stripe-signature", format!("t={t},v1={}", s.to_uppercase()))]);
        assert_eq!(
            verify(&v, Some(KEY), &upper, body, NOW).unwrap().replay_key,
            key
        );
        assert!(
            verify(&v, Some(KEY), &h, body, NOW + 301)
                .unwrap_err()
                .contains("timestamp")
        );

        let s = hex(&sign("sha256", KEY, &[b"v0:", t.as_bytes(), b":", body]));
        let v = decl(serde_json::json!({ "scheme": "slack", "secret": { "secret": "s" } }));
        let h = headers(&[
            ("x-slack-request-timestamp", t.clone()),
            ("x-slack-signature", format!("v0={s}")),
        ]);
        assert!(verify(&v, Some(KEY), &h, body, NOW).is_ok());
        assert!(verify(&v, Some(KEY), &h, body, NOW - 1000).is_err());

        let key = B64.encode(KEY);
        let s = B64.encode(sign("sha256", KEY, &[b"msg_1.", t.as_bytes(), b".", body]));
        let v = decl(serde_json::json!({ "scheme": "standard", "secret": { "secret": "s" } }));
        let h = headers(&[
            ("webhook-id", "msg_1".into()),
            ("webhook-timestamp", t.clone()),
            ("webhook-signature", format!("v1,bogus v1,{s}")),
        ]);
        let ok = verify(&v, Some(format!("whsec_{key}").as_bytes()), &h, body, NOW).unwrap();
        assert_eq!(
            (ok.replay_key, ok.delivery_id.as_deref()),
            (replay_key("standard", &[b"msg_1"]), Some("msg_1"))
        );
    }

    #[test]
    fn generic_hmac() {
        let body = b"payload";
        let v = decl(serde_json::json!({
            "scheme": "hmac", "secret": { "secret": "s" }, "algorithm": "sha1",
            "header": "X-Sig", "prefix": "sha1=", "deliveryHeader": "X-Id"
        }));
        let s = hex(&sign("sha1", KEY, &[body]));
        let h = headers(&[("x-sig", format!("sha1={s}")), ("x-id", "7".into())]);
        let ok = verify(&v, Some(KEY), &h, body, NOW).unwrap();
        assert_eq!(
            (ok.replay_key, ok.delivery_id.as_deref()),
            (replay_key("hmac", &[body]), Some("7"))
        );
        let v = decl(serde_json::json!({
            "scheme": "hmac", "secret": { "secret": "s" }, "algorithm": "sha512", "encoding": "base64",
            "timestampHeader": "X-Ts"
        }));
        let t = NOW.to_string();
        let s = B64.encode(sign("sha512", KEY, &[t.as_bytes(), b".", body]));
        let h = headers(&[("x-signature", s.clone()), ("x-ts", t.clone())]);
        assert_eq!(
            verify(&v, Some(KEY), &h, body, NOW).unwrap().replay_key,
            replay_key("hmac", &[t.as_bytes(), body])
        );
        assert!(verify(&v, Some(KEY), &h, b"other", NOW).is_err());
        assert_eq!(
            verify(
                &decl(serde_json::json!({ "scheme": "none" })),
                None,
                &HeaderMap::new(),
                body,
                NOW
            )
            .unwrap(),
            Verified::default()
        );
    }
}
