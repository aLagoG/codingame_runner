//! Per-bot lineage manifest (`bot.toml`).
//!
//! Each bot crate carries a `bot.toml` next to its `Cargo.toml` recording
//! the data the iteration workflow can't infer from source: who its
//! parent was (for the clone-and-tweak loop), who's the current champion
//! per (game, lang) pair (so `bundle` defaults to "the bot we ship today"),
//! and the running history of tournaments this bot participated in.
//!
//! This is the load-bearing primitive every Tier-2 verb reads from —
//! `retire` checks "is anyone's parent" before deleting; `promote` walks
//! sibling chains; `compare` resolves bot names to crate paths via this
//! file. Keep the schema small + stable; new fields go as optional with
//! `#[serde(default)]` so old manifests parse forward.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// On-disk schema for `games/<game>/bots/<bot>_<lang>/bot.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotManifest {
    /// Bot stem, without language suffix — i.e. the directory name minus
    /// `_<lang>`. For `games/fantastic_bits/bots/v1_5_cpp/bot.toml` this
    /// is `"v1_5"`.
    pub name: String,
    /// Language suffix matching the directory naming convention:
    /// `"cpp"` or `"rs"`. Used by lineage walkers to resolve sibling
    /// crates within the same lang lane.
    pub lang: String,
    /// Stem of the parent bot in the same game/lang. `None` for
    /// originally-authored baselines that weren't cloned from anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// RFC-3339 timestamp of when this bot was scaffolded. Optional —
    /// backfilled manifests omit it (we don't fabricate a fake date for
    /// pre-existing bots; the field is informational anyway).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// One-line summary of what this bot is. For freshly-scaffolded
    /// bots, defaults to a generic "<game> bot (<lang>)" string; for
    /// `--from-existing` clones, defaults to "clone of <parent>". The
    /// user is meant to overwrite it with something meaningful.
    pub description: String,
    /// CodinGame league the bot reached when last submitted, e.g.
    /// `"Wood 2"`, `"Bronze"`, `"Silver"`, `"Gold"`, `"Legend"`. Free-
    /// form to track CG's evolving league naming. `None` until the
    /// user submits the bot and fills it in by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codingame_league: Option<String>,
    /// CodinGame standing (rank) within `codingame_league` at last
    /// submission. `None` until the user submits and fills it in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codingame_standing: Option<u32>,
    /// At most one bot per (game, lang) should have `champion = true`
    /// — the bot `bundle` / runner default to when no name is given.
    /// `promote` flips this bit; backfills set it on the currently-
    /// shipped bot. Multiple champions is a soft error surfaced by the
    /// promote/retire verbs (the report tool doesn't enforce).
    #[serde(default)]
    pub champion: bool,
    /// Append-only log of tournament outcomes involving this bot.
    /// Populated by future `iterate` / `compare` runs. Empty by default;
    /// readers can ignore.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryEntry>,
}

/// One tournament outcome (typically appended automatically by
/// `iterate` / `compare`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub ran_at: String,
    pub opponent: String,
    pub rounds: u32,
    pub pts: f64,
    pub opponent_pts: f64,
    /// `"significant"` | `"inconclusive"` | `"worse"` — matches the
    /// vocabulary `tournament compare` will print.
    pub verdict: String,
}

impl BotManifest {
    /// Build the path `games/<game>/bots/<bot>_<lang>/bot.toml`.
    pub fn path(game: &str, bot: &str, lang: &str) -> PathBuf {
        PathBuf::from("games")
            .join(game)
            .join("bots")
            .join(format!("{bot}_{lang}"))
            .join("bot.toml")
    }

    /// Read a manifest from disk. Errors if the file doesn't exist or
    /// fails to parse — callers handling missing manifests should
    /// check `path().exists()` first.
    pub fn read(path: &Path) -> Result<BotManifest> {
        let s =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&s).with_context(|| format!("parsing {}", path.display()))
    }

    /// Write the manifest to disk, creating parent dirs as needed.
    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let body = toml::to_string_pretty(self)
            .with_context(|| format!("serializing manifest for {}", path.display()))?;
        std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// Current RFC-3339 timestamp (`YYYY-MM-DDTHH:MM:SSZ`). Uses
/// `SystemTime` rather than a date crate to keep xtask deps small.
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Cheap RFC-3339 (UTC, no sub-second precision) without chrono.
    // Civil-from-epoch via Howard Hinnant's algorithm, paraphrased.
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (h, m, s) = (
        secs_of_day / 3_600,
        (secs_of_day / 60) % 60,
        secs_of_day % 60,
    );
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Days since Unix epoch → (year, month, day). Howard Hinnant's
/// public-domain algorithm: handles dates from -5,877,641-06-23 to
/// +5,879,610-09-09 without leap-year bugs.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i32 + (era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_minimal_manifest() {
        let m = BotManifest {
            name: "v1_5".into(),
            lang: "cpp".into(),
            parent: Some("v1".into()),
            created_at: None,
            description: "post-aware Flipendo".into(),
            codingame_league: None,
            codingame_standing: None,
            champion: false,
            history: vec![],
        };
        let s = toml::to_string_pretty(&m).unwrap();
        // Optional fields are omitted from the rendered form.
        assert!(!s.contains("created_at"));
        assert!(!s.contains("codingame_league"));
        assert!(!s.contains("codingame_standing"));
        assert!(!s.contains("history"));
        let back: BotManifest = toml::from_str(&s).unwrap();
        assert_eq!(back.name, "v1_5");
        assert_eq!(back.parent.as_deref(), Some("v1"));
        assert!(back.codingame_league.is_none());
        assert!(back.codingame_standing.is_none());
        assert!(!back.champion);
    }

    #[test]
    fn round_trip_with_codingame_submission_info() {
        let m = BotManifest {
            name: "v1".into(),
            lang: "cpp".into(),
            parent: None,
            created_at: None,
            description: "post-aware Flipendo".into(),
            codingame_league: Some("Bronze".into()),
            codingame_standing: Some(412),
            champion: true,
            history: vec![],
        };
        let s = toml::to_string_pretty(&m).unwrap();
        assert!(s.contains("codingame_league = \"Bronze\""));
        assert!(s.contains("codingame_standing = 412"));
        let back: BotManifest = toml::from_str(&s).unwrap();
        assert_eq!(back.codingame_league.as_deref(), Some("Bronze"));
        assert_eq!(back.codingame_standing, Some(412));
    }

    #[test]
    fn now_rfc3339_shape() {
        let s = now_rfc3339();
        // 2026-06-01T00:00:00Z — 20 chars.
        assert_eq!(s.len(), 20);
        assert!(s.ends_with('Z'));
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
    }
}
