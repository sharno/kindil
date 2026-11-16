//! Settings → Location: where the dashboard is. Found by a city search
//! (Open-Meteo's geocoder), from the internet address (ipinfo.io) or typed
//! as coordinates, then written into Miqat's `[location]` and kindil's own
//! `timezone`, comments and all.
use anyhow::Context;
use chrono_tz::Tz;
use miqat::config::{Config as Miqat, Latitude, Longitude};
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;

/// A place the dashboard can be at.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    /// "Amsterdam, North Holland, Netherlands"; None for typed coordinates.
    pub name: Option<String>,
    pub latitude: f64,
    pub longitude: f64,
    pub timezone: Tz,
}

impl Place {
    /// Its name, else its coordinates to two decimals.
    pub fn title(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| coordinates(self.latitude, self.longitude))
    }

    /// "Amsterdam, North Holland, Netherlands · Europe/Amsterdam".
    pub fn summary(&self) -> String {
        format!("{} · {}", self.title(), self.timezone.name())
    }

    /// Miqat's own checks: the radar map stops at ±85°.
    fn usable(latitude: f64, longitude: f64) -> Result<(), String> {
        Latitude::try_from(latitude)?;
        Longitude::try_from(longitude)?;
        Ok(())
    }
}

/// "52.37, 4.90".
pub fn coordinates(latitude: f64, longitude: f64) -> String {
    format!("{latitude:.2}, {longitude:.2}")
}

/// "Name, Region, Country", leaving out empty parts and a region that only
/// repeats the name.
fn label(parts: [Option<&str>; 3]) -> Option<String> {
    let [name, region, country] = parts.map(|p| p.map(str::trim).filter(|p| !p.is_empty()));
    let region = region.filter(|r| Some(*r) != name);
    let label: Vec<&str> = [name, region, country].into_iter().flatten().collect();
    (!label.is_empty()).then(|| label.join(", "))
}

// ── Open-Meteo's geocoder ──

pub const SEARCH_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";

#[derive(Deserialize)]
struct Search {
    /// Absent when nothing matched.
    #[serde(default)]
    results: Vec<Hit>,
    #[serde(default)]
    error: bool,
    reason: Option<String>,
}

#[derive(Deserialize)]
struct Hit {
    name: String,
    latitude: f64,
    longitude: f64,
    timezone: Option<String>,
    admin1: Option<String>,
    country: Option<String>,
}

/// The places in a search answer. Ones without a time zone kindil knows, or
/// too near a pole for Miqat, are left out.
pub fn parse_search(json: &str) -> anyhow::Result<Vec<Place>> {
    let search: Search = serde_json::from_str(json).context("unexpected answer")?;
    if search.error {
        anyhow::bail!(search.reason.unwrap_or_else(|| "the search failed".into()));
    }
    Ok(search
        .results
        .into_iter()
        .filter_map(|hit| {
            let timezone = hit.timezone.as_deref()?.parse().ok()?;
            Place::usable(hit.latitude, hit.longitude).ok()?;
            Some(Place {
                name: label([
                    Some(&hit.name),
                    hit.admin1.as_deref(),
                    hit.country.as_deref(),
                ]),
                latitude: hit.latitude,
                longitude: hit.longitude,
                timezone,
            })
        })
        .collect())
}

// ── ipinfo.io ──

pub const IPINFO_URL: &str = "https://ipinfo.io/json";

#[derive(Deserialize)]
struct IpInfo {
    city: Option<String>,
    region: Option<String>,
    country: Option<String>,
    /// "52.3740,4.8897".
    loc: Option<String>,
    timezone: Option<String>,
    /// A private address: ipinfo knows nothing about it.
    #[serde(default)]
    bogon: bool,
    error: Option<serde_json::Value>,
}

/// Where ipinfo places the internet address it saw.
pub fn parse_ipinfo(json: &str) -> anyhow::Result<Place> {
    let info: IpInfo = serde_json::from_str(json).context("unexpected answer")?;
    if let Some(error) = info.error {
        let message = error
            .get("message")
            .or_else(|| error.get("title"))
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| error.to_string(), str::to_owned);
        anyhow::bail!(message);
    }
    anyhow::ensure!(!info.bogon, "the internet address is a private one");
    let (latitude, longitude) = info
        .loc
        .as_deref()
        .and_then(|loc| loc.split_once(','))
        .and_then(|(lat, lon)| Some((lat.trim().parse().ok()?, lon.trim().parse().ok()?)))
        .context("no coordinates in the answer")?;
    Place::usable(latitude, longitude).map_err(anyhow::Error::msg)?;
    let timezone = info
        .timezone
        .as_deref()
        .context("no time zone in the answer")?;
    let timezone = timezone
        .parse()
        .map_err(|_| anyhow::anyhow!("unknown time zone {timezone:?}"))?;
    Ok(Place {
        name: label([
            info.city.as_deref(),
            info.region.as_deref(),
            info.country.as_deref(),
        ]),
        latitude,
        longitude,
        timezone,
    })
}

// ── over HTTPS ──

/// Both services, with no token; asked only when the owner taps.
pub struct Lookup {
    agent: ureq::Agent,
}

impl Default for Lookup {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .user_agent(concat!("kindil/", env!("CARGO_PKG_VERSION")))
            // A 4xx answer carries the reason in its body.
            .http_status_as_error(false)
            .build()
            .into();
        Self { agent }
    }
}

/// Answers are small; anything bigger isn't one.
const MAX_ANSWER: u64 = 1 << 20;

impl Lookup {
    fn read(
        &self,
        host: &str,
        response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> anyhow::Result<(ureq::http::StatusCode, String)> {
        let mut response = response.with_context(|| format!("can't reach {host}"))?;
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_ANSWER)
            .read_to_string()
            .with_context(|| format!("reading {host}'s answer"))?;
        Ok((response.status(), body))
    }

    /// Up to ten places whose name starts with `name`.
    pub fn search(&self, name: &str) -> anyhow::Result<Vec<Place>> {
        let name = name.trim();
        anyhow::ensure!(name.chars().count() >= 2, "type at least two letters");
        let host = "geocoding-api.open-meteo.com";
        let (status, body) = self.read(
            host,
            self.agent
                .get(SEARCH_URL)
                .query("name", name)
                .query("count", "10")
                .query("language", "en")
                .query("format", "json")
                .call(),
        )?;
        let places = parse_search(&body).with_context(|| format!("{host} ({status})"))?;
        anyhow::ensure!(status.is_success(), "{host} answered {status}");
        Ok(places)
    }

    /// Where the internet address seems to be.
    pub fn locate(&self) -> anyhow::Result<Place> {
        let host = "ipinfo.io";
        let (status, body) = self.read(host, self.agent.get(IPINFO_URL).call())?;
        let place = parse_ipinfo(&body).with_context(|| format!("{host} ({status})"))?;
        anyhow::ensure!(status.is_success(), "{host} answered {status}");
        Ok(place)
    }
}

// ── typed coordinates ──

/// Typed latitude, longitude and time zone, checked as Miqat would.
pub fn typed(latitude: &str, longitude: &str, timezone: &str) -> Result<Place, String> {
    let number = |text: &str, what: &str| {
        text.trim()
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .ok_or_else(|| format!("{what} must be a number in degrees, e.g. 52.37"))
    };
    let latitude = number(latitude, "Latitude")?;
    let longitude = number(longitude, "Longitude")?;
    Latitude::try_from(latitude).map_err(|_| "Latitude must be between -85 and 85".to_string())?;
    Longitude::try_from(longitude)
        .map_err(|_| "Longitude must be between -180 and 180".to_string())?;
    let timezone = timezone
        .trim()
        .parse()
        .map_err(|_| format!("Unknown time zone “{}”", timezone.trim()))?;
    Ok(Place {
        name: None,
        latitude,
        longitude,
        timezone,
    })
}

/// Time zones whose name holds what was typed ("new york" finds
/// America/New_York), at most `limit`, in tzdb's order.
pub fn zones(typed: &str, limit: usize) -> Vec<&'static str> {
    let needle = typed.trim().to_lowercase().replace(' ', "_");
    chrono_tz::TZ_VARIANTS
        .iter()
        .map(|tz| tz.name())
        // Region/City names, as Open-Meteo and ipinfo give them, and UTC.
        .filter(|name| (name.contains('/') && !name.starts_with("Etc/")) || *name == "UTC")
        .filter(|name| name.to_lowercase().contains(&needle))
        .take(limit)
        .collect()
}

// ── the files ──

/// Four decimals is about ten metres: plenty for prayer times and weather.
fn rounded(degrees: f64) -> f64 {
    (degrees * 10_000.0).round() / 10_000.0
}

/// Sets `key` in `table`, keeping the old value's spacing and comment.
fn set(table: &mut dyn toml_edit::TableLike, key: &str, value: toml_edit::Value) {
    match table.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
        Some(old) => {
            let decor = old.decor().clone();
            *old = value;
            *old.decor_mut() = decor;
        }
        None => {
            table.insert(key, toml_edit::Item::Value(value));
        }
    }
}

/// Miqat's file with `place` as its `[location]`; an error if the text isn't
/// a config Miqat accepts, before or after.
pub fn edit_miqat(text: &str, place: &Place) -> anyhow::Result<String> {
    toml::from_str::<Miqat>(text)?;
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let location = doc
        .get_mut("location")
        .and_then(toml_edit::Item::as_table_like_mut)
        .context("no [location] table")?;
    let (latitude, longitude) = (rounded(place.latitude), rounded(place.longitude));
    set(location, "latitude", latitude.into());
    set(location, "longitude", longitude.into());
    set(location, "timezone", place.timezone.name().into());
    let text = doc.to_string();
    let parsed: Miqat = toml::from_str(&text)?;
    let at = parsed.location;
    anyhow::ensure!(
        at.latitude.degrees() == latitude
            && at.longitude.degrees() == longitude
            && at.timezone == place.timezone,
        "[location] did not change"
    );
    Ok(text)
}

/// kindil's file with `timezone` for the clock; None: there is no file yet,
/// and it will hold just that.
pub fn edit_kindil(text: Option<&str>, timezone: Tz) -> anyhow::Result<String> {
    let text = text.unwrap_or("");
    crate::config::Config::parse(text)?;
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    set(doc.as_table_mut(), "timezone", timezone.name().into());
    let text = doc.to_string();
    let parsed = crate::config::Config::parse(&text)?;
    anyhow::ensure!(parsed.timezone == timezone, "timezone did not change");
    Ok(text)
}

/// Writes `place` into Miqat's file and its time zone into kindil's. Both
/// are checked and written beside the originals first, so a missing or
/// broken file, or a full card, leaves both as they were.
pub fn save(miqat: &Path, kindil: &Path, place: &Place) -> anyhow::Result<()> {
    anyhow::ensure!(
        !kindil.as_os_str().is_empty(),
        "kindil.toml's path is unknown"
    );
    let miqat_text = edit_miqat(
        &std::fs::read_to_string(miqat).with_context(|| format!("reading {}", miqat.display()))?,
        place,
    )
    .with_context(|| format!("parsing {}", miqat.display()))?;
    let kindil_text = match std::fs::read_to_string(kindil) {
        Ok(text) => edit_kindil(Some(&text), place.timezone),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            edit_kindil(None, place.timezone)
        }
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", kindil.display()));
        }
    }
    .with_context(|| format!("parsing {}", kindil.display()))?;
    let staged = (|| {
        let a = crate::prayer::stage(miqat, &miqat_text)?;
        match crate::prayer::stage(kindil, &kindil_text) {
            Ok(b) => Ok((a, b)),
            Err(error) => {
                let _ = std::fs::remove_file(&a);
                Err(error)
            }
        }
    })()
    .context("writing the settings")?;
    std::fs::rename(&staged.0, miqat).with_context(|| format!("writing {}", miqat.display()))?;
    std::fs::rename(&staged.1, kindil).with_context(|| format!("writing {}", kindil.display()))
}

/// The dashboard's place as Miqat's file has it, named if `named` (saved
/// in prefs) is at the same coordinates.
pub fn load(miqat: &Path, named: Option<&crate::prefs::Named>) -> anyhow::Result<Place> {
    let location = Miqat::load(miqat)?.location;
    let (latitude, longitude) = (location.latitude.degrees(), location.longitude.degrees());
    Ok(Place {
        name: named
            .filter(|n| n.latitude == latitude && n.longitude == longitude)
            .map(|n| n.name.clone()),
        latitude,
        longitude,
        timezone: location.timezone,
    })
}

/// What prefs keep of a saved place: its name, and where it was, so a
/// hand-edited `[location]` isn't shown under the old name.
pub fn named(place: &Place) -> Option<crate::prefs::Named> {
    Some(crate::prefs::Named {
        name: place.name.clone()?,
        latitude: rounded(place.latitude),
        longitude: rounded(place.longitude),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An abridged answer for "Amsterdam".
    const AMSTERDAM: &str = r#"{"results":[
        {"id":2759794,"name":"Amsterdam","latitude":52.37403,"longitude":4.88969,
         "elevation":13.0,"feature_code":"PPLC","country_code":"NL",
         "timezone":"Europe/Amsterdam","population":741636,"country":"Netherlands",
         "admin1":"North Holland","admin2":"Gemeente Amsterdam"},
        {"id":5107129,"name":"Amsterdam","latitude":42.93869,"longitude":-74.18819,
         "country_code":"US","timezone":"America/New_York","country":"United States",
         "admin1":"New York","admin2":"Montgomery"},
        {"id":1,"name":"Amsterdam Station","latitude":-89.9,"longitude":0.0,
         "timezone":"Antarctica/McMurdo","country":"Antarctica"},
        {"id":2,"name":"Amsterdam Island","latitude":-37.8,"longitude":77.5}
    ],"generationtime_ms":0.6}"#;

    /// ipinfo's answer without a token, for a documentation address.
    const IPINFO: &str = r#"{"ip":"203.0.113.7","city":"Amsterdam","region":"North Holland",
        "country":"NL","loc":"52.3740,4.8897","org":"AS64496 Example","postal":"1012",
        "timezone":"Europe/Amsterdam","readme":"https://ipinfo.io/missingauth"}"#;

    const MECCA: &str = r#"# Where the dashboard is.
[location]
latitude = 21.4225   # degrees north
longitude = 39.8262
timezone = "Asia/Riyadh"   # IANA name

[prayer]
method = "umm_al_qura"   # the mosque's choice
"#;

    const KINDIL: &str = r#"# kindil settings.
dashboard = "miqat"
timezone = "Asia/Riyadh"        # IANA name, for the clock

# [syncthing]
# binary = "/mnt/us/kindil/syncthing"
"#;

    fn amsterdam() -> Place {
        Place {
            name: Some("Amsterdam, North Holland, Netherlands".into()),
            latitude: 52.37403,
            longitude: 4.88969,
            timezone: chrono_tz::Europe::Amsterdam,
        }
    }

    #[test]
    fn search_answers_list_places_with_their_zones() {
        let places = parse_search(AMSTERDAM).unwrap();
        assert_eq!(places.len(), 2, "no zone or past 85°: left out");
        assert_eq!(places[0], amsterdam());
        assert_eq!(
            places[1].summary(),
            "Amsterdam, New York, United States · America/New_York"
        );
        assert_eq!(parse_search(r#"{"generationtime_ms":0.2}"#).unwrap(), []);
        let error =
            parse_search(r#"{"error":true,"reason":"Parameter count must be between 1 and 100."}"#)
                .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Parameter count must be between 1 and 100."
        );
        assert!(parse_search("<html>Bad gateway</html>").is_err());
    }

    #[test]
    fn a_region_that_repeats_the_name_is_left_out() {
        let json = r#"{"results":[{"name":"New York","latitude":40.71427,"longitude":-74.00597,
            "timezone":"America/New_York","admin1":"New York","country":"United States"},
            {"name":"Mecca","latitude":21.42664,"longitude":39.82563,"timezone":"Asia/Riyadh",
            "admin1":"","country":"Saudi Arabia"}]}"#;
        let places = parse_search(json).unwrap();
        assert_eq!(places[0].title(), "New York, United States");
        assert_eq!(places[1].title(), "Mecca, Saudi Arabia");
    }

    #[test]
    fn ipinfo_answers_give_the_place_and_zone() {
        let place = parse_ipinfo(IPINFO).unwrap();
        assert_eq!(
            place.summary(),
            "Amsterdam, North Holland, NL · Europe/Amsterdam"
        );
        assert_eq!((place.latitude, place.longitude), (52.374, 4.8897));
        assert!(parse_ipinfo(r#"{"ip":"10.0.0.2","bogon":true}"#).is_err());
        let limited = r#"{"status":429,"error":{"title":"Rate limit exceeded","message":"Upgrade to increase your usage limits."}}"#;
        assert_eq!(
            parse_ipinfo(limited).unwrap_err().to_string(),
            "Upgrade to increase your usage limits."
        );
        assert!(parse_ipinfo(&IPINFO.replace("52.3740,4.8897", "nowhere")).is_err());
        assert!(parse_ipinfo(&IPINFO.replace("Europe/Amsterdam", "Mars/Olympus")).is_err());
    }

    #[test]
    fn typed_coordinates_are_checked_as_miqat_would() {
        let place = typed(" 21.4225", "39.8262 ", "Asia/Riyadh").unwrap();
        assert_eq!(place.summary(), "21.42, 39.83 · Asia/Riyadh");
        assert_eq!(place.name, None);
        assert!(typed("85", "-180", "UTC").is_ok());
        assert_eq!(
            typed("85.1", "0", "UTC").unwrap_err(),
            "Latitude must be between -85 and 85"
        );
        assert_eq!(
            typed("0", "180.5", "UTC").unwrap_err(),
            "Longitude must be between -180 and 180"
        );
        assert!(typed("north", "0", "UTC").is_err());
        assert!(typed("NaN", "0", "UTC").is_err());
        assert!(typed("", "0", "UTC").is_err());
        assert_eq!(
            typed("1", "2", "Europe/Atlantis").unwrap_err(),
            "Unknown time zone “Europe/Atlantis”"
        );
    }

    #[test]
    fn zones_are_found_by_any_part_of_their_name() {
        assert_eq!(zones("new york", 5), ["America/New_York"]);
        assert!(zones("amster", 5).contains(&"Europe/Amsterdam"));
        assert_eq!(zones("", 3).len(), 3);
        assert!(zones("utc", 50).contains(&"UTC"));
        assert!(!zones("", 1000).iter().any(|z| z.starts_with("Etc/")));
        assert_eq!(zones("zzzz", 5), Vec::<&str>::new());
    }

    #[test]
    fn both_files_change_and_keep_their_comments() {
        let text = edit_miqat(MECCA, &amsterdam()).unwrap();
        assert_eq!(
            text,
            MECCA
                .replace("21.4225   # degrees north", "52.374   # degrees north")
                .replace("39.8262", "4.8897")
                .replace("\"Asia/Riyadh\"   # IANA", "\"Europe/Amsterdam\"   # IANA")
        );
        let kindil = edit_kindil(Some(KINDIL), chrono_tz::Europe::Amsterdam).unwrap();
        assert_eq!(
            kindil,
            KINDIL.replace(
                "\"Asia/Riyadh\"        # IANA",
                "\"Europe/Amsterdam\"        # IANA"
            )
        );
        // No key yet: added at the top, not under a table.
        let tables = "[syncthing]\nbinary = '/a'\nhome = '/b'\n";
        let added = edit_kindil(Some(tables), chrono_tz::America::New_York).unwrap();
        let config = crate::config::Config::parse(&added).unwrap();
        assert_eq!(config.timezone, chrono_tz::America::New_York);
        assert!(config.syncthing.is_some());
        assert_eq!(
            edit_kindil(None, chrono_tz::UTC).unwrap(),
            "timezone = \"UTC\"\n"
        );
    }

    #[test]
    fn saving_writes_both_files_and_creates_kindils() {
        let dir = tempfile::tempdir().unwrap();
        let (miqat, kindil) = (
            dir.path().join("miqat.toml"),
            dir.path().join("kindil.toml"),
        );
        std::fs::write(&miqat, MECCA).unwrap();
        save(&miqat, &kindil, &amsterdam()).unwrap();
        let place = load(&miqat, named(&amsterdam()).as_ref()).unwrap();
        assert_eq!(place.summary(), amsterdam().summary());
        assert_eq!(
            std::fs::read_to_string(&kindil).unwrap(),
            "timezone = \"Europe/Amsterdam\"\n"
        );
        assert!(!miqat.with_extension("toml.tmp").exists());
        assert!(!kindil.with_extension("toml.tmp").exists());
        // A name saved for elsewhere isn't shown for a hand-edited place.
        let mecca = typed("21.4225", "39.8262", "Asia/Riyadh").unwrap();
        save(&miqat, &kindil, &mecca).unwrap();
        let place = load(&miqat, named(&amsterdam()).as_ref()).unwrap();
        assert_eq!(place.summary(), "21.42, 39.83 · Asia/Riyadh");
    }

    #[test]
    fn errors_leave_both_files_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let (miqat, kindil) = (
            dir.path().join("miqat.toml"),
            dir.path().join("kindil.toml"),
        );
        let place = amsterdam();
        // Miqat's file missing.
        std::fs::write(&kindil, KINDIL).unwrap();
        assert!(save(&miqat, &kindil, &place).is_err());
        assert!(!miqat.exists());
        assert_eq!(std::fs::read_to_string(&kindil).unwrap(), KINDIL);
        // Miqat's file broken, or with a key Miqat rejects.
        for broken in [
            MECCA.replace("[prayer]", "[prayer"),
            MECCA.replace("longitude", "longtitude"),
        ] {
            std::fs::write(&miqat, &broken).unwrap();
            let error = save(&miqat, &kindil, &place).unwrap_err();
            assert!(format!("{error:#}").contains("parsing"), "{error:#}");
            assert_eq!(std::fs::read_to_string(&miqat).unwrap(), broken);
            assert_eq!(std::fs::read_to_string(&kindil).unwrap(), KINDIL);
        }
        // kindil's file broken: Miqat's is left as it was too.
        std::fs::write(&miqat, MECCA).unwrap();
        let broken = KINDIL.replace("dashboard = \"miqat\"", "dashboard = \"radio\"");
        std::fs::write(&kindil, &broken).unwrap();
        assert!(save(&miqat, &kindil, &place).is_err());
        assert_eq!(std::fs::read_to_string(&miqat).unwrap(), MECCA);
        assert_eq!(std::fs::read_to_string(&kindil).unwrap(), broken);
        // kindil's path unknown.
        assert!(save(&miqat, Path::new(""), &place).is_err());
        assert_eq!(std::fs::read_to_string(&miqat).unwrap(), MECCA);
        assert!(!miqat.with_extension("toml.tmp").exists());
    }

    /// The real services, with public place names only.
    #[test]
    #[ignore = "network"]
    fn the_real_search_finds_mecca() {
        let places = Lookup::default().search("Mecca").unwrap();
        let mecca = places
            .iter()
            .find(|p| p.timezone == chrono_tz::Asia::Riyadh)
            .unwrap();
        assert!((mecca.latitude - 21.42).abs() < 0.1, "{mecca:?}");
        assert!(Lookup::default().search("Qqqqxzzv").unwrap().is_empty());
    }
}
