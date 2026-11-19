//! Durable topics. Messages are kept in order. Each subscription has a cursor and gets the messages
//! after it one at a time, in order: an ack moves the cursor on, a nack delivers the message again
//! after a wait that doubles, and after too many deliveries it becomes a dead letter and the cursor
//! moves past it. A dead letter can be delivered to its subscription again, or discarded.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::error::{Error, Result};

/// Deliveries of a message to a subscription before it is a dead letter.
pub const MAX_ATTEMPTS: i32 = 10;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Message {
    pub seq: i64,
    pub topic: String,
    pub payload: Value,
    pub published_by: String,
    pub published_at: DateTime<Utc>,
}

/// What a subscription gets next.
#[derive(Debug, Clone, PartialEq)]
pub enum Next {
    /// A message, and the how-manyth delivery this is.
    Message(Message, i32),
    /// Nothing before then: a nacked message waits.
    Wait(DateTime<Utc>),
    /// Nothing until something is published.
    Empty,
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Subscription {
    pub subscription: String,
    pub acked: i64,
    pub attempts: i32,
    pub lag: i64,
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct DeadLetter {
    pub subscription: String,
    pub seq: i64,
    pub attempts: i32,
    pub error: Option<String>,
    pub retry: bool,
    pub at: DateTime<Utc>,
    pub payload: Value,
}

/// Where a new subscription starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    /// With the next message published.
    Latest,
    /// With the first message kept.
    Start,
}

pub struct Topics<'a> {
    pub(crate) pool: &'a PgPool,
}

const MSG: &str = "m.seq, m.topic, m.payload, m.published_by, m.published_at";

impl Topics<'_> {
    pub async fn publish(&self, topic: &str, payload: &Value, by: &str) -> Result<i64> {
        Ok(sqlx::query_scalar("INSERT INTO rookery.topic_messages (topic, payload, published_by) VALUES ($1, $2, $3) RETURNING seq")
            .bind(topic)
            .bind(payload)
            .bind(by)
            .fetch_one(self.pool)
            .await?)
    }

    /// Makes a subscription if there is none.
    pub async fn subscribe(&self, topic: &str, subscription: &str, from: From) -> Result<()> {
        sqlx::query(
            "INSERT INTO rookery.topic_subscriptions (topic, subscription, acked)
             VALUES ($1, $2, CASE WHEN $3 THEN coalesce((SELECT max(seq) FROM rookery.topic_messages WHERE topic = $1), 0) ELSE 0 END)
             ON CONFLICT DO NOTHING",
        )
        .bind(topic)
        .bind(subscription)
        .bind(from == From::Latest)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    /// The message a subscription gets next: a dead letter to deliver again, else the one after its cursor.
    pub async fn next(&self, topic: &str, subscription: &str) -> Result<Next> {
        let retried: Option<(Message, i32)> = sqlx::query_as::<_, (i64, String, Value, String, DateTime<Utc>, i32)>(sqlx::AssertSqlSafe(format!(
            "SELECT {MSG}, d.attempts FROM rookery.dead_letters d JOIN rookery.topic_messages m ON m.seq = d.seq
             WHERE d.topic = $1 AND d.subscription = $2 AND d.retry ORDER BY d.seq LIMIT 1"
        )))
        .bind(topic)
        .bind(subscription)
        .fetch_optional(self.pool)
        .await?
        .map(|(seq, topic, payload, published_by, published_at, attempts)| {
            (Message { seq, topic, payload, published_by, published_at }, attempts + 1)
        });
        if let Some((message, attempt)) = retried {
            return Ok(Next::Message(message, attempt));
        }
        let state: Option<(i64, i32, Option<DateTime<Utc>>)> =
            sqlx::query_as("SELECT acked, attempts, not_before FROM rookery.topic_subscriptions WHERE topic = $1 AND subscription = $2")
                .bind(topic)
                .bind(subscription)
                .fetch_optional(self.pool)
                .await?;
        let Some((acked, attempts, not_before)) = state else {
            return Err(Error::NotFound(format!(
                "no subscription {subscription} of {topic}"
            )));
        };
        let message: Option<Message> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {MSG} FROM rookery.topic_messages m WHERE m.topic = $1 AND m.seq > $2 ORDER BY m.seq LIMIT 1"
        )))
        .bind(topic)
        .bind(acked)
        .fetch_optional(self.pool)
        .await?;
        match (message, not_before) {
            (None, _) => Ok(Next::Empty),
            (Some(_), Some(at)) if at > Utc::now() => Ok(Next::Wait(at)),
            (Some(message), _) => Ok(Next::Message(message, attempts + 1)),
        }
    }

    /// The subscription is done with a message.
    pub async fn ack(&self, topic: &str, subscription: &str, seq: i64) -> Result<()> {
        let retried = sqlx::query("DELETE FROM rookery.dead_letters WHERE topic = $1 AND subscription = $2 AND seq = $3 AND retry")
            .bind(topic)
            .bind(subscription)
            .bind(seq)
            .execute(self.pool)
            .await?;
        if retried.rows_affected() > 0 {
            return Ok(());
        }
        let done = sqlx::query(
            "UPDATE rookery.topic_subscriptions SET acked = $3, attempts = 0, not_before = NULL, last_error = NULL
             WHERE topic = $1 AND subscription = $2 AND acked < $3",
        )
        .bind(topic)
        .bind(subscription)
        .bind(seq)
        .execute(self.pool)
        .await?;
        if done.rows_affected() == 0 {
            return Err(Error::NotFound(format!(
                "message {seq} is not waiting for {subscription}"
            )));
        }
        Ok(())
    }

    /// The subscription could not handle a message: it comes again after `backoff` doubled for each
    /// delivery, or becomes a dead letter after [`MAX_ATTEMPTS`]. Returns whether it is dead.
    pub async fn nack(
        &self,
        topic: &str,
        subscription: &str,
        seq: i64,
        error: Option<&str>,
        backoff: std::time::Duration,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let retried: Option<i32> = sqlx::query_scalar(
            "UPDATE rookery.dead_letters SET retry = false, attempts = attempts + 1, error = $4, at = now()
             WHERE topic = $1 AND subscription = $2 AND seq = $3 AND retry RETURNING attempts",
        )
        .bind(topic)
        .bind(subscription)
        .bind(seq)
        .bind(error)
        .fetch_optional(&mut *tx)
        .await?;
        if retried.is_some() {
            tx.commit().await?;
            return Ok(true);
        }
        let attempts: Option<i32> = sqlx::query_scalar(
            "UPDATE rookery.topic_subscriptions SET attempts = attempts + 1, last_error = $4
             WHERE topic = $1 AND subscription = $2 AND acked < $3 RETURNING attempts",
        )
        .bind(topic)
        .bind(subscription)
        .bind(seq)
        .bind(error)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(attempts) = attempts else {
            return Err(Error::NotFound(format!(
                "message {seq} is not waiting for {subscription}"
            )));
        };
        let dead = attempts >= MAX_ATTEMPTS;
        if dead {
            sqlx::query(
                "INSERT INTO rookery.dead_letters (topic, subscription, seq, attempts, error) VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (topic, subscription, seq) DO UPDATE SET attempts = EXCLUDED.attempts, error = EXCLUDED.error, at = now()",
            )
            .bind(topic)
            .bind(subscription)
            .bind(seq)
            .bind(attempts)
            .bind(error)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE rookery.topic_subscriptions SET acked = $3, attempts = 0, not_before = NULL, last_error = NULL
                 WHERE topic = $1 AND subscription = $2",
            )
            .bind(topic)
            .bind(subscription)
            .bind(seq)
            .execute(&mut *tx)
            .await?;
        } else {
            let wait = (backoff.as_secs_f64() * 2f64.powi(attempts - 1)).min(60.0);
            sqlx::query(
                "UPDATE rookery.topic_subscriptions SET not_before = now() + make_interval(secs => $3) WHERE topic = $1 AND subscription = $2",
            )
            .bind(topic)
            .bind(subscription)
            .bind(wait)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(dead)
    }

    /// The newest message of a topic, how many it keeps, and its subscriptions with their lag.
    pub async fn inspect(&self, topic: &str) -> Result<(i64, i64, Vec<Subscription>, i64)> {
        let (head, count): (Option<i64>, i64) = sqlx::query_as(
            "SELECT max(seq), count(*) FROM rookery.topic_messages WHERE topic = $1",
        )
        .bind(topic)
        .fetch_one(self.pool)
        .await?;
        let subscriptions = sqlx::query_as(
            "SELECT s.subscription, s.acked, s.attempts,
                    (SELECT count(*) FROM rookery.topic_messages m WHERE m.topic = s.topic AND m.seq > s.acked) AS lag
             FROM rookery.topic_subscriptions s WHERE s.topic = $1 ORDER BY s.subscription",
        )
        .bind(topic)
        .fetch_all(self.pool)
        .await?;
        let dead: i64 =
            sqlx::query_scalar("SELECT count(*) FROM rookery.dead_letters WHERE topic = $1")
                .bind(topic)
                .fetch_one(self.pool)
                .await?;
        Ok((head.unwrap_or(0), count, subscriptions, dead))
    }

    pub async fn dead_letters(
        &self,
        topic: &str,
        subscription: Option<&str>,
    ) -> Result<Vec<DeadLetter>> {
        Ok(sqlx::query_as(
            "SELECT d.subscription, d.seq, d.attempts, d.error, d.retry, d.at, m.payload
             FROM rookery.dead_letters d JOIN rookery.topic_messages m ON m.seq = d.seq
             WHERE d.topic = $1 AND ($2::text IS NULL OR d.subscription = $2) ORDER BY d.seq",
        )
        .bind(topic)
        .bind(subscription)
        .fetch_all(self.pool)
        .await?)
    }

    /// Delivers a dead letter to its subscription again.
    pub async fn retry(&self, topic: &str, subscription: &str, seq: i64) -> Result<()> {
        let done = sqlx::query("UPDATE rookery.dead_letters SET retry = true WHERE topic = $1 AND subscription = $2 AND seq = $3")
            .bind(topic)
            .bind(subscription)
            .bind(seq)
            .execute(self.pool)
            .await?;
        if done.rows_affected() == 0 {
            return Err(Error::NotFound(format!(
                "no dead letter {seq} of {subscription}"
            )));
        }
        Ok(())
    }

    pub async fn discard(&self, topic: &str, subscription: &str, seq: i64) -> Result<bool> {
        let done = sqlx::query(
            "DELETE FROM rookery.dead_letters WHERE topic = $1 AND subscription = $2 AND seq = $3",
        )
        .bind(topic)
        .bind(subscription)
        .bind(seq)
        .execute(self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }
}
