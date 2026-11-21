// Prayer times from Adhan (adhan-js), for tests/differential.rs.
//
//   git clone https://github.com/batoulapps/adhan-js /tmp/adhan-js
//   git -C /tmp/adhan-js checkout d2993d56fac7d8a06abba4837347bd3fc9cbd188
//   TZ=UTC bun tests/fixtures/adhan/generate.ts /tmp/adhan-js > tests/fixtures/adhan/times.csv
//
// First a line per place: "place,NAME,LATITUDE,LONGITUDE". Then a line per
// case: the place's name, the date, method, madhab, high-latitude rule,
// shafaq and polar circle resolution, then Adhan's fajr, sunrise, dhuhr,
// asr, maghrib and isha, and its SunnahTimes' middle and last third of the
// night, in minutes after the date's 00:00 UTC, or "-" when Adhan has no
// times.

const adhan = process.argv[2];
const A = await import(`${adhan}/src/Adhan.ts`);

const places: [string, number, number][] = [
  ['Makkah', 21.4225, 39.8262],
  ['Cairo', 30.0444, 31.2357],
  ['Istanbul', 41.0082, 28.9784],
  ['Tehran', 35.6892, 51.389],
  ['Karachi', 24.8607, 67.0011],
  ['Dhaka', 23.8103, 90.4125],
  ['Jakarta', -6.2088, 106.8456],
  ['Kuala Lumpur', 3.139, 101.6869],
  ['Tokyo', 35.6762, 139.6503],
  ['Sydney', -33.8688, 151.2093],
  ['Auckland', -36.8485, 174.7633],
  ['Suva', -18.1416, 178.4419],
  ['Kiritimati', 1.8721, -157.4278],
  ['Honolulu', 21.3069, -157.8583],
  ['Anchorage', 61.2181, -149.9003],
  ['Mexico City', 19.4326, -99.1332],
  ['Lima', -12.0464, -77.0428],
  ['Buenos Aires', -34.6037, -58.3816],
  ['Ushuaia', -54.8019, -68.303],
  ['Dakar', 14.7167, -17.4677],
  ['Cape Town', -33.9249, 18.4241],
  ['London', 51.5074, -0.1278],
  ['Paris', 48.8566, 2.3522],
  ['Oslo', 59.9139, 10.7522],
  ['Reykjavik', 64.1466, -21.9426],
  ['Tromso', 69.6492, 18.9553],
  ['Murmansk', 68.9585, 33.0827],
  ['Longyearbyen', 78.2232, 15.6267],
  ['McMurdo', -77.846, 166.676],
];

const methods = [
  'MuslimWorldLeague', 'Egyptian', 'Karachi', 'UmmAlQura', 'Dubai',
  'MoonsightingCommittee', 'NorthAmerica', 'Kuwait', 'Qatar', 'Singapore',
  'Tehran', 'Turkey',
];

// Every method as it comes (Shafi), and the variations on the Muslim World
// League's and the Moonsighting Committee's.
const settings: [string, string, string, string, string][] = [
  ...methods.map((m): [string, string, string, string, string] =>
    [m, 'shafi', 'middleofthenight', 'general', 'Unresolved']),
  ['MuslimWorldLeague', 'hanafi', 'middleofthenight', 'general', 'Unresolved'],
  ['MuslimWorldLeague', 'shafi', 'seventhofthenight', 'general', 'Unresolved'],
  ['MuslimWorldLeague', 'shafi', 'twilightangle', 'general', 'Unresolved'],
  ['MuslimWorldLeague', 'shafi', 'middleofthenight', 'general', 'AqrabBalad'],
  ['MuslimWorldLeague', 'shafi', 'middleofthenight', 'general', 'AqrabYaum'],
  ['MoonsightingCommittee', 'hanafi', 'middleofthenight', 'ahmer', 'Unresolved'],
  ['MoonsightingCommittee', 'shafi', 'middleofthenight', 'abyad', 'Unresolved'],
];

// Every 19th day of 2026, the solstices and equinoxes, and a few other
// years.
const dates: [number, number, number][] = [];
for (let day = 1; day <= 365; day += 19) {
  const d = new Date(Date.UTC(2026, 0, day));
  dates.push([d.getUTCFullYear(), d.getUTCMonth() + 1, d.getUTCDate()]);
}
dates.push([2026, 3, 20], [2026, 6, 21], [2026, 9, 23], [2026, 12, 21]);
dates.push([1990, 2, 14], [2000, 1, 1], [2012, 7, 1], [2050, 10, 10], [2100, 4, 4]);

const minutes = (time: Date, midnight: number) =>
  isNaN(time.getTime()) ? '-' : String(Math.round((time.getTime() - midnight) / 60000));

const lines: string[] = places.map(([name, lat, lon]) => `place,${name},${lat},${lon}`);
for (const [name, lat, lon] of places) {
  for (const [y, m, d] of dates) {
    for (const [method, madhab, rule, shafaq, polar] of settings) {
      const p = (A.CalculationMethod as any)[method]();
      p.madhab = madhab;
      p.highLatitudeRule = rule;
      p.shafaq = shafaq;
      p.polarCircleResolution = polar;
      const times = new A.PrayerTimes(new A.Coordinates(lat, lon), new Date(y, m - 1, d), p);
      const midnight = Date.UTC(y, m - 1, d);
      const six = ['fajr', 'sunrise', 'dhuhr', 'asr', 'maghrib', 'isha']
        .map((k) => minutes((times as any)[k], midnight));
      let night = ['-', '-'];
      if (times.isValid) {
        const s = new A.SunnahTimes(times);
        night = [minutes(s.middleOfTheNight, midnight), minutes(s.lastThirdOfTheNight, midnight)];
      }
      lines.push([name, y, m, d, method, madhab, rule, shafaq, polar, ...six, ...night].join(','));
    }
  }
}
console.log(lines.join('\n'));
