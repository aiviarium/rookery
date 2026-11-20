-- Whether a person wants a kind as a native notification, beside whether they mute it.
ALTER TABLE rookery.notification_prefs ADD COLUMN native boolean NOT NULL DEFAULT false;
