//! Local persistence for the model catalogue.
//!
//! The catalogue is a cache of an external document, so this table holds exactly
//! one row: the snapshot in force. It is not a history — nothing derives from an
//! old snapshot once a newer one lands, and keeping stale payloads around would
//! only invite pricing a call against a version nobody chose.

use rusqlite::{Connection, OptionalExtension};

use super::{Catalog, Source};

/// Read the cached snapshot, if there is one.
pub fn read(conn: &Connection) -> anyhow::Result<Option<Catalog>> {
    let row = conn
        .query_row(
            "SELECT digest, etag, fetched_at, checked_at, payload
               FROM model_catalog ORDER BY fetched_at DESC LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()?;

    let Some((digest, etag, fetched_at, checked_at, payload)) = row else {
        return Ok(None);
    };

    let json = super::ungzip(&payload)?;
    let mut catalog = super::parse(&json, etag, Source::Cached)?;
    // Keep the stored timestamps: when this payload was downloaded is a fact
    // about the cache, not about this process starting up.
    catalog.snapshot.digest = digest;
    catalog.snapshot.fetched_at = fetched_at;
    catalog.snapshot.checked_at = checked_at;
    Ok(Some(catalog))
}

/// The digest and ETag of the cached snapshot, for a conditional request.
pub fn current_etag(conn: &Connection) -> anyhow::Result<Option<(String, Option<String>)>> {
    Ok(conn
        .query_row(
            "SELECT digest, etag FROM model_catalog ORDER BY fetched_at DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

/// Store a freshly fetched payload, replacing whatever was cached.
pub fn write(
    conn: &Connection,
    digest: &str,
    etag: Option<&str>,
    payload: &[u8],
) -> anyhow::Result<()> {
    let now = crate::util::now_rfc3339();
    let squashed = super::gzip(payload)?;
    conn.execute("DELETE FROM model_catalog", [])?;
    conn.execute(
        "INSERT INTO model_catalog (digest, etag, fetched_at, checked_at, payload)
         VALUES (?1, ?2, ?3, ?3, ?4)",
        rusqlite::params![digest, etag, now, squashed],
    )?;
    Ok(())
}

/// Record that the cached snapshot was confirmed current.
///
/// A 304 means the payload we hold is still the published one, which is worth
/// knowing — otherwise a catalogue that never changes looks indistinguishable
/// from one that stopped being checked.
pub fn touch(conn: &Connection) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE model_catalog SET checked_at = ?1",
        [crate::util::now_rfc3339()],
    )?;
    Ok(())
}
