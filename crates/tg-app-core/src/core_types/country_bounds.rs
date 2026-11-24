//! Country geographic bounding boxes and centre-point resolution for the location picker.
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/raw/raw_countries_bounds.cpp` (`Raw::CountryBounds`)
//! - `Telegram/SourceFiles/core/current_geo_location.cpp` (`Core::ResolveCurrentCountryLocation`)
//!
//! Note: tdesktop's `raw_countries_bounds.cpp` imports `sandstrom/country-bounding-boxes` tuples
//! ordered as `(min_lon, min_lat, max_lon, max_lat)` (GeoJSON `[west, south, east, north]`).
//! [`CountryGeoBounds`] stores the decoded WGS84 fields explicitly as `min_lat`, `min_lon`,
//! `max_lat`, `max_lon` so callers receive standard `(latitude, longitude)` pairs.

/// WGS84 bounding box for an ISO 3166-1 alpha-2 country.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CountryGeoBounds {
    /// Southernmost latitude in WGS84 degrees (`-90..=90`).
    pub min_lat: f64,
    /// Westernmost longitude in WGS84 degrees (`-180..=180`).
    pub min_lon: f64,
    /// Northernmost latitude in WGS84 degrees (`-90..=90`).
    pub max_lat: f64,
    /// Easternmost longitude in WGS84 degrees (`-180..=180`).
    pub max_lon: f64,
}

impl CountryGeoBounds {
    /// Creates a [`CountryGeoBounds`] from tdesktop's GeoJSON `[west, south, east, north]`
    /// (`min_lon, min_lat, max_lon, max_lat`) tuple order.
    const fn from_wse_n(min_lon: f64, min_lat: f64, max_lon: f64, max_lat: f64) -> Self {
        Self {
            min_lat,
            min_lon,
            max_lat,
            max_lon,
        }
    }

    /// Returns the midpoint `(latitude, longitude)` of the country bounding box, clamped to valid
    /// Web Mercator / Telegram static map latitude bounds (`-85.0511..=85.0511`, `-180.0..=180.0`).
    #[must_use]
    pub const fn center(self) -> (f64, f64) {
        let lat = ((self.min_lat + self.max_lat) * 0.5).clamp(-85.0511, 85.0511);
        let lon = ((self.min_lon + self.max_lon) * 0.5).clamp(-180.0, 180.0);
        (lat, lon)
    }
}

/// Complete port of `Raw::CountryBounds()` from `data/raw/raw_countries_bounds.cpp`.
const COUNTRY_BOUNDS: &[(&str, CountryGeoBounds)] = &[
    (
        "AF",
        CountryGeoBounds::from_wse_n(60.53, 29.32, 75.16, 38.49),
    ),
    (
        "AO",
        CountryGeoBounds::from_wse_n(11.64, -17.93, 24.08, -4.44),
    ),
    (
        "AL",
        CountryGeoBounds::from_wse_n(19.3, 39.62, 21.02, 42.69),
    ),
    ("AE", CountryGeoBounds::from_wse_n(51.58, 22.5, 56.4, 26.06)),
    (
        "AR",
        CountryGeoBounds::from_wse_n(-73.42, -55.25, -53.63, -21.83),
    ),
    (
        "AM",
        CountryGeoBounds::from_wse_n(43.58, 38.74, 46.51, 41.25),
    ),
    (
        "AQ",
        CountryGeoBounds::from_wse_n(-180.0, -90.0, 180.0, -63.27),
    ),
    (
        "TF",
        CountryGeoBounds::from_wse_n(68.72, -49.78, 70.56, -48.63),
    ),
    (
        "AU",
        CountryGeoBounds::from_wse_n(113.34, -43.63, 153.57, -10.67),
    ),
    (
        "AT",
        CountryGeoBounds::from_wse_n(9.48, 46.43, 16.98, 49.04),
    ),
    (
        "AZ",
        CountryGeoBounds::from_wse_n(44.79, 38.27, 50.39, 41.86),
    ),
    (
        "BI",
        CountryGeoBounds::from_wse_n(29.02, -4.5, 30.75, -2.35),
    ),
    ("BE", CountryGeoBounds::from_wse_n(2.51, 49.53, 6.16, 51.48)),
    ("BJ", CountryGeoBounds::from_wse_n(0.77, 6.14, 3.8, 12.24)),
    ("BF", CountryGeoBounds::from_wse_n(-5.47, 9.61, 2.18, 15.12)),
    (
        "BD",
        CountryGeoBounds::from_wse_n(88.08, 20.67, 92.67, 26.45),
    ),
    (
        "BG",
        CountryGeoBounds::from_wse_n(22.38, 41.23, 28.56, 44.23),
    ),
    (
        "BS",
        CountryGeoBounds::from_wse_n(-78.98, 23.71, -77.0, 27.04),
    ),
    (
        "BA",
        CountryGeoBounds::from_wse_n(15.75, 42.65, 19.6, 45.23),
    ),
    (
        "BY",
        CountryGeoBounds::from_wse_n(23.2, 51.32, 32.69, 56.17),
    ),
    (
        "BZ",
        CountryGeoBounds::from_wse_n(-89.23, 15.89, -88.11, 18.5),
    ),
    (
        "BO",
        CountryGeoBounds::from_wse_n(-69.59, -22.87, -57.5, -9.76),
    ),
    (
        "BR",
        CountryGeoBounds::from_wse_n(-73.99, -33.77, -34.73, 5.24),
    ),
    (
        "BN",
        CountryGeoBounds::from_wse_n(114.2, 4.01, 115.45, 5.45),
    ),
    ("BT", CountryGeoBounds::from_wse_n(88.81, 26.72, 92.1, 28.3)),
    (
        "BW",
        CountryGeoBounds::from_wse_n(19.9, -26.83, 29.43, -17.66),
    ),
    (
        "CF",
        CountryGeoBounds::from_wse_n(14.46, 2.27, 27.37, 11.14),
    ),
    (
        "CA",
        CountryGeoBounds::from_wse_n(-141.0, 41.68, -52.65, 73.23),
    ),
    (
        "CH",
        CountryGeoBounds::from_wse_n(6.02, 45.78, 10.44, 47.83),
    ),
    (
        "CL",
        CountryGeoBounds::from_wse_n(-75.64, -55.61, -66.96, -17.58),
    ),
    (
        "CN",
        CountryGeoBounds::from_wse_n(73.68, 18.2, 135.03, 53.46),
    ),
    ("CI", CountryGeoBounds::from_wse_n(-8.6, 4.34, -2.56, 10.52)),
    ("CM", CountryGeoBounds::from_wse_n(8.49, 1.73, 16.01, 12.86)),
    (
        "CD",
        CountryGeoBounds::from_wse_n(12.18, -13.26, 31.17, 5.26),
    ),
    (
        "CG",
        CountryGeoBounds::from_wse_n(11.09, -5.04, 18.45, 3.73),
    ),
    (
        "CO",
        CountryGeoBounds::from_wse_n(-78.99, -4.3, -66.88, 12.44),
    ),
    (
        "CR",
        CountryGeoBounds::from_wse_n(-85.94, 8.23, -82.55, 11.22),
    ),
    (
        "CU",
        CountryGeoBounds::from_wse_n(-84.97, 19.86, -74.18, 23.19),
    ),
    (
        "CY",
        CountryGeoBounds::from_wse_n(32.26, 34.57, 34.0, 35.17),
    ),
    (
        "CZ",
        CountryGeoBounds::from_wse_n(12.24, 48.56, 18.85, 51.12),
    ),
    ("DE", CountryGeoBounds::from_wse_n(5.99, 47.3, 15.02, 54.98)),
    (
        "DJ",
        CountryGeoBounds::from_wse_n(41.66, 10.93, 43.32, 12.7),
    ),
    ("DK", CountryGeoBounds::from_wse_n(8.09, 54.8, 12.69, 57.73)),
    (
        "DO",
        CountryGeoBounds::from_wse_n(-71.95, 17.6, -68.32, 19.88),
    ),
    (
        "DZ",
        CountryGeoBounds::from_wse_n(-8.68, 19.06, 12.0, 37.12),
    ),
    (
        "EC",
        CountryGeoBounds::from_wse_n(-80.97, -4.96, -75.23, 1.38),
    ),
    ("EG", CountryGeoBounds::from_wse_n(24.7, 22.0, 36.87, 31.59)),
    (
        "ER",
        CountryGeoBounds::from_wse_n(36.32, 12.46, 43.08, 18.0),
    ),
    (
        "ES",
        CountryGeoBounds::from_wse_n(-9.39, 35.95, 3.04, 43.75),
    ),
    (
        "EE",
        CountryGeoBounds::from_wse_n(23.34, 57.47, 28.13, 59.61),
    ),
    (
        "ET",
        CountryGeoBounds::from_wse_n(32.95, 3.42, 47.79, 14.96),
    ),
    (
        "FI",
        CountryGeoBounds::from_wse_n(20.65, 59.85, 31.52, 70.16),
    ),
    (
        "FJ",
        CountryGeoBounds::from_wse_n(-180.0, -18.29, 180.0, -16.02),
    ),
    (
        "FK",
        CountryGeoBounds::from_wse_n(-61.2, -52.3, -57.75, -51.1),
    ),
    ("FR", CountryGeoBounds::from_wse_n(-5.0, 42.5, 9.56, 51.15)),
    ("GA", CountryGeoBounds::from_wse_n(8.8, -3.98, 14.43, 2.33)),
    (
        "GB",
        CountryGeoBounds::from_wse_n(-7.57, 49.96, 1.68, 58.64),
    ),
    (
        "GE",
        CountryGeoBounds::from_wse_n(39.96, 41.06, 46.64, 43.55),
    ),
    ("GH", CountryGeoBounds::from_wse_n(-3.24, 4.71, 1.06, 11.1)),
    (
        "GN",
        CountryGeoBounds::from_wse_n(-15.13, 7.31, -7.83, 12.59),
    ),
    (
        "GM",
        CountryGeoBounds::from_wse_n(-16.84, 13.13, -13.84, 13.88),
    ),
    (
        "GW",
        CountryGeoBounds::from_wse_n(-16.68, 11.04, -13.7, 12.63),
    ),
    ("GQ", CountryGeoBounds::from_wse_n(9.31, 1.01, 11.29, 2.28)),
    (
        "GR",
        CountryGeoBounds::from_wse_n(20.15, 34.92, 26.6, 41.83),
    ),
    (
        "GL",
        CountryGeoBounds::from_wse_n(-73.3, 60.04, -12.21, 83.65),
    ),
    (
        "GT",
        CountryGeoBounds::from_wse_n(-92.23, 13.74, -88.23, 17.82),
    ),
    (
        "GY",
        CountryGeoBounds::from_wse_n(-61.41, 1.27, -56.54, 8.37),
    ),
    (
        "HN",
        CountryGeoBounds::from_wse_n(-89.35, 12.98, -83.15, 16.01),
    ),
    (
        "HR",
        CountryGeoBounds::from_wse_n(13.66, 42.48, 19.39, 46.5),
    ),
    (
        "HT",
        CountryGeoBounds::from_wse_n(-74.46, 18.03, -71.62, 19.92),
    ),
    (
        "HU",
        CountryGeoBounds::from_wse_n(16.2, 45.76, 22.71, 48.62),
    ),
    (
        "ID",
        CountryGeoBounds::from_wse_n(95.29, -10.36, 141.03, 5.48),
    ),
    ("IN", CountryGeoBounds::from_wse_n(68.18, 7.97, 97.4, 35.49)),
    (
        "IE",
        CountryGeoBounds::from_wse_n(-9.98, 51.67, -6.03, 55.13),
    ),
    (
        "IR",
        CountryGeoBounds::from_wse_n(44.11, 25.08, 63.32, 39.71),
    ),
    (
        "IQ",
        CountryGeoBounds::from_wse_n(38.79, 29.1, 48.57, 37.39),
    ),
    (
        "IS",
        CountryGeoBounds::from_wse_n(-24.33, 63.5, -13.61, 66.53),
    ),
    (
        "IL",
        CountryGeoBounds::from_wse_n(34.27, 29.5, 35.84, 33.28),
    ),
    (
        "IT",
        CountryGeoBounds::from_wse_n(6.75, 36.62, 18.48, 47.12),
    ),
    (
        "JM",
        CountryGeoBounds::from_wse_n(-78.34, 17.7, -76.2, 18.52),
    ),
    ("JO", CountryGeoBounds::from_wse_n(34.92, 29.2, 39.2, 33.38)),
    (
        "JP",
        CountryGeoBounds::from_wse_n(129.41, 31.03, 145.54, 45.55),
    ),
    (
        "KZ",
        CountryGeoBounds::from_wse_n(46.47, 40.66, 87.36, 55.39),
    ),
    (
        "KE",
        CountryGeoBounds::from_wse_n(33.89, -4.68, 41.86, 5.51),
    ),
    (
        "KG",
        CountryGeoBounds::from_wse_n(69.46, 39.28, 80.26, 43.3),
    ),
    (
        "KH",
        CountryGeoBounds::from_wse_n(102.35, 10.49, 107.61, 14.57),
    ),
    (
        "KR",
        CountryGeoBounds::from_wse_n(126.12, 34.39, 129.47, 38.61),
    ),
    (
        "KW",
        CountryGeoBounds::from_wse_n(46.57, 28.53, 48.42, 30.06),
    ),
    (
        "LA",
        CountryGeoBounds::from_wse_n(100.12, 13.88, 107.56, 22.46),
    ),
    (
        "LB",
        CountryGeoBounds::from_wse_n(35.13, 33.09, 36.61, 34.64),
    ),
    (
        "LR",
        CountryGeoBounds::from_wse_n(-11.44, 4.36, -7.54, 8.54),
    ),
    (
        "LY",
        CountryGeoBounds::from_wse_n(9.32, 19.58, 25.16, 33.14),
    ),
    ("LK", CountryGeoBounds::from_wse_n(79.7, 5.97, 81.79, 9.82)),
    (
        "LS",
        CountryGeoBounds::from_wse_n(27.0, -30.65, 29.33, -28.65),
    ),
    (
        "LT",
        CountryGeoBounds::from_wse_n(21.06, 53.91, 26.59, 56.37),
    ),
    ("LU", CountryGeoBounds::from_wse_n(5.67, 49.44, 6.24, 50.13)),
    (
        "LV",
        CountryGeoBounds::from_wse_n(21.06, 55.62, 28.18, 57.97),
    ),
    (
        "MA",
        CountryGeoBounds::from_wse_n(-17.02, 21.42, -1.12, 35.76),
    ),
    (
        "MD",
        CountryGeoBounds::from_wse_n(26.62, 45.49, 30.02, 48.47),
    ),
    (
        "MG",
        CountryGeoBounds::from_wse_n(43.25, -25.6, 50.48, -12.04),
    ),
    (
        "MX",
        CountryGeoBounds::from_wse_n(-117.13, 14.54, -86.81, 32.72),
    ),
    (
        "MK",
        CountryGeoBounds::from_wse_n(20.46, 40.84, 22.95, 42.32),
    ),
    (
        "ML",
        CountryGeoBounds::from_wse_n(-12.17, 10.1, 4.27, 24.97),
    ),
    (
        "MM",
        CountryGeoBounds::from_wse_n(92.3, 9.93, 101.18, 28.34),
    ),
    (
        "ME",
        CountryGeoBounds::from_wse_n(18.45, 41.88, 20.34, 43.52),
    ),
    (
        "MN",
        CountryGeoBounds::from_wse_n(87.75, 41.6, 119.77, 52.05),
    ),
    (
        "MZ",
        CountryGeoBounds::from_wse_n(30.18, -26.74, 40.78, -10.32),
    ),
    (
        "MR",
        CountryGeoBounds::from_wse_n(-17.06, 14.62, -4.92, 27.4),
    ),
    (
        "MW",
        CountryGeoBounds::from_wse_n(32.69, -16.8, 35.77, -9.23),
    ),
    (
        "MY",
        CountryGeoBounds::from_wse_n(100.09, 0.77, 119.18, 6.93),
    ),
    (
        "NA",
        CountryGeoBounds::from_wse_n(11.73, -29.05, 25.08, -16.94),
    ),
    (
        "NC",
        CountryGeoBounds::from_wse_n(164.03, -22.4, 167.12, -20.11),
    ),
    ("NE", CountryGeoBounds::from_wse_n(0.3, 11.66, 15.9, 23.47)),
    ("NG", CountryGeoBounds::from_wse_n(2.69, 4.24, 14.58, 13.87)),
    (
        "NI",
        CountryGeoBounds::from_wse_n(-87.67, 10.73, -83.15, 15.02),
    ),
    ("NL", CountryGeoBounds::from_wse_n(3.31, 50.8, 7.09, 53.51)),
    (
        "NO",
        CountryGeoBounds::from_wse_n(4.99, 58.08, 31.29, 70.92),
    ),
    (
        "NP",
        CountryGeoBounds::from_wse_n(80.09, 26.4, 88.17, 30.42),
    ),
    (
        "NZ",
        CountryGeoBounds::from_wse_n(166.51, -46.64, 178.52, -34.45),
    ),
    ("OM", CountryGeoBounds::from_wse_n(52.0, 16.65, 59.81, 26.4)),
    (
        "PK",
        CountryGeoBounds::from_wse_n(60.87, 23.69, 77.84, 37.13),
    ),
    (
        "PA",
        CountryGeoBounds::from_wse_n(-82.97, 7.22, -77.24, 9.61),
    ),
    (
        "PE",
        CountryGeoBounds::from_wse_n(-81.41, -18.35, -68.67, -0.06),
    ),
    (
        "PH",
        CountryGeoBounds::from_wse_n(117.17, 5.58, 126.54, 18.51),
    ),
    (
        "PG",
        CountryGeoBounds::from_wse_n(141.0, -10.65, 156.02, -2.5),
    ),
    (
        "PL",
        CountryGeoBounds::from_wse_n(14.07, 49.03, 24.03, 54.85),
    ),
    (
        "PR",
        CountryGeoBounds::from_wse_n(-67.24, 17.95, -65.59, 18.52),
    ),
    (
        "KP",
        CountryGeoBounds::from_wse_n(124.27, 37.67, 130.78, 42.99),
    ),
    (
        "PT",
        CountryGeoBounds::from_wse_n(-9.53, 36.84, -6.39, 42.28),
    ),
    (
        "PY",
        CountryGeoBounds::from_wse_n(-62.69, -27.55, -54.29, -19.34),
    ),
    (
        "QA",
        CountryGeoBounds::from_wse_n(50.74, 24.56, 51.61, 26.11),
    ),
    (
        "RO",
        CountryGeoBounds::from_wse_n(20.22, 43.69, 29.63, 48.22),
    ),
    (
        "RU",
        CountryGeoBounds::from_wse_n(-18.0, 41.15, 180.0, 81.25),
    ),
    (
        "RW",
        CountryGeoBounds::from_wse_n(29.02, -2.92, 30.82, -1.13),
    ),
    (
        "SA",
        CountryGeoBounds::from_wse_n(34.63, 16.35, 55.67, 32.16),
    ),
    ("SD", CountryGeoBounds::from_wse_n(21.94, 8.62, 38.41, 22.0)),
    ("SS", CountryGeoBounds::from_wse_n(23.89, 3.51, 35.3, 12.25)),
    (
        "SN",
        CountryGeoBounds::from_wse_n(-17.63, 12.33, -11.47, 16.6),
    ),
    (
        "SB",
        CountryGeoBounds::from_wse_n(156.49, -10.83, 162.4, -6.6),
    ),
    (
        "SL",
        CountryGeoBounds::from_wse_n(-13.25, 6.79, -10.23, 10.05),
    ),
    (
        "SV",
        CountryGeoBounds::from_wse_n(-90.1, 13.15, -87.72, 14.42),
    ),
    (
        "SO",
        CountryGeoBounds::from_wse_n(40.98, -1.68, 51.13, 12.02),
    ),
    (
        "RS",
        CountryGeoBounds::from_wse_n(18.83, 42.25, 22.99, 46.17),
    ),
    (
        "SR",
        CountryGeoBounds::from_wse_n(-58.04, 1.82, -53.96, 6.03),
    ),
    (
        "SK",
        CountryGeoBounds::from_wse_n(16.88, 47.76, 22.56, 49.57),
    ),
    (
        "SI",
        CountryGeoBounds::from_wse_n(13.7, 45.45, 16.56, 46.85),
    ),
    (
        "SE",
        CountryGeoBounds::from_wse_n(11.03, 55.36, 23.9, 69.11),
    ),
    (
        "SZ",
        CountryGeoBounds::from_wse_n(30.68, -27.29, 32.07, -25.66),
    ),
    (
        "SY",
        CountryGeoBounds::from_wse_n(35.7, 32.31, 42.35, 37.23),
    ),
    (
        "TD",
        CountryGeoBounds::from_wse_n(13.54, 7.42, 23.89, 23.41),
    ),
    ("TG", CountryGeoBounds::from_wse_n(-0.05, 5.93, 1.87, 11.02)),
    (
        "TH",
        CountryGeoBounds::from_wse_n(97.38, 5.69, 105.59, 20.42),
    ),
    (
        "TJ",
        CountryGeoBounds::from_wse_n(67.44, 36.74, 74.98, 40.96),
    ),
    (
        "TM",
        CountryGeoBounds::from_wse_n(52.5, 35.27, 66.55, 42.75),
    ),
    (
        "TL",
        CountryGeoBounds::from_wse_n(124.97, -9.39, 127.34, -8.27),
    ),
    (
        "TT",
        CountryGeoBounds::from_wse_n(-61.95, 10.0, -60.9, 10.89),
    ),
    (
        "TN",
        CountryGeoBounds::from_wse_n(7.52, 30.31, 11.49, 37.35),
    ),
    (
        "TR",
        CountryGeoBounds::from_wse_n(26.04, 35.82, 44.79, 42.14),
    ),
    (
        "TW",
        CountryGeoBounds::from_wse_n(120.11, 21.97, 121.95, 25.3),
    ),
    (
        "TZ",
        CountryGeoBounds::from_wse_n(29.34, -11.72, 40.32, -0.95),
    ),
    (
        "UG",
        CountryGeoBounds::from_wse_n(29.58, -1.44, 35.04, 4.25),
    ),
    (
        "UA",
        CountryGeoBounds::from_wse_n(22.09, 44.36, 40.08, 52.34),
    ),
    (
        "UY",
        CountryGeoBounds::from_wse_n(-58.43, -34.95, -53.21, -30.11),
    ),
    (
        "US",
        CountryGeoBounds::from_wse_n(-125.0, 25.0, -66.96, 49.5),
    ),
    (
        "UZ",
        CountryGeoBounds::from_wse_n(55.93, 37.14, 73.06, 45.59),
    ),
    (
        "VE",
        CountryGeoBounds::from_wse_n(-73.3, 0.72, -59.76, 12.16),
    ),
    (
        "VN",
        CountryGeoBounds::from_wse_n(102.17, 8.6, 109.34, 23.35),
    ),
    (
        "VU",
        CountryGeoBounds::from_wse_n(166.63, -16.6, 167.84, -14.63),
    ),
    (
        "PS",
        CountryGeoBounds::from_wse_n(34.93, 31.35, 35.55, 32.53),
    ),
    ("YE", CountryGeoBounds::from_wse_n(42.6, 12.59, 53.11, 19.0)),
    (
        "ZA",
        CountryGeoBounds::from_wse_n(16.34, -34.82, 32.83, -22.09),
    ),
    (
        "ZM",
        CountryGeoBounds::from_wse_n(21.89, -17.96, 33.49, -8.24),
    ),
    (
        "ZW",
        CountryGeoBounds::from_wse_n(25.26, -22.27, 32.85, -15.51),
    ),
];

/// Looks up tdesktop's bounding box for a 2-letter ISO 3166-1 alpha-2 country code.
#[must_use]
pub fn country_bounds(iso2: &str) -> Option<CountryGeoBounds> {
    let trimmed = iso2.trim();
    if trimmed.len() != 2 {
        return None;
    }
    let upper = trimmed.to_ascii_uppercase();
    COUNTRY_BOUNDS
        .iter()
        .find(|(code, _)| *code == upper)
        .map(|(_, bounds)| *bounds)
}

/// Returns the `(latitude, longitude)` centre of `iso2` from tdesktop's country bounds table.
#[must_use]
pub fn country_center(iso2: &str) -> Option<(f64, f64)> {
    country_bounds(iso2).map(CountryGeoBounds::center)
}

/// Resolves an ISO 3166-1 alpha-2 country code from a Telegram account phone number using
/// `tg-lang`'s country calling-code registry, returning `Some(iso2)` only when the code maps to a
/// country present in [`country_bounds`].
#[must_use]
pub fn country_iso2_from_phone(phone: &str) -> Option<String> {
    let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let iso2 = {
        let guard = tg_lang::countries::global_countries()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.country_iso2_by_phone(&digits)
    };
    let upper = iso2.trim().to_ascii_uppercase();
    if country_bounds(&upper).is_some() {
        Some(upper)
    } else {
        None
    }
}

/// Returns the human-readable English country name for `iso2` when known by `tg-lang`.
#[must_use]
pub fn country_display_name(iso2: &str) -> Option<String> {
    let upper = iso2.trim().to_ascii_uppercase();
    if upper.len() != 2 {
        return None;
    }
    let guard = tg_lang::countries::global_countries()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let name = guard.country_name_by_iso2(&upper, tg_lang::countries::Naming::Default);
    if name.is_empty() { None } else { Some(name) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn country_center_matches_tdesktop_bounds() {
        let (us_lat, us_lon) = country_center("us").expect("US bounds");
        assert!((us_lat - 37.25).abs() < 1e-6);
        assert!((us_lon - (-95.98)).abs() < 1e-6);

        let (de_lat, de_lon) = country_center("DE").expect("DE bounds");
        assert!((de_lat - 51.14).abs() < 1e-6);
        assert!((de_lon - 10.505).abs() < 1e-6);

        let (jp_lat, jp_lon) = country_center("JP").expect("JP bounds");
        assert!((jp_lat - 38.29).abs() < 1e-6);
        assert!((jp_lon - 137.475).abs() < 1e-6);

        assert!(country_center("ZZ").is_none());
        assert!(country_center("").is_none());
    }

    #[test]
    fn country_iso2_from_phone_resolves_calling_codes() {
        assert_eq!(
            country_iso2_from_phone("+49 30 5550199").as_deref(),
            Some("DE")
        );
        assert_eq!(
            country_iso2_from_phone("380501112233").as_deref(),
            Some("UA")
        );
        assert_eq!(
            country_iso2_from_phone("+81 3 5555 0100").as_deref(),
            Some("JP")
        );
        assert_eq!(country_iso2_from_phone(""), None);
    }
}
