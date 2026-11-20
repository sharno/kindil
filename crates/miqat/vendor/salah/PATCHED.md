# salah 0.7.6, vendored and patched

[salah](https://github.com/insha/salah) 0.7.6 (MIT, see LICENSE), the
latest release, as published on crates.io, with its `spectral`
dev-dependency and the Qiblah tests that used it removed.

The patch (marked "kindil's patch" in the code): salah 0.7.6 unwrapped
the NaN of a sun that never reaches an angle on a day, so it panicked

- wherever the sun stays above Fajr's or Isha's angle all night: from
  about 49°N (18°, the Muslim World League's) in the weeks around the
  June solstice, Amsterdam included;
- wherever it doesn't rise or set at all (past the polar circles).

Now, as in Adhan (which salah ports), a twilight never reached gives way
to the method's high-latitude share of the night ("safe" Fajr and Isha),
and a day without sunrise or sunset is `PrayerSchedule::calculate`'s
`Err` (`PrayerTimes::try_new`'s `None`). `SolarTime::new`,
`time_for_solar_angle` and `afternoon` return `Option`; `setting_hour`
takes any finite hour, midnight's 0.0 included.
