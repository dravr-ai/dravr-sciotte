// ABOUTME: Activity data models mirroring dravr-platform pierre-core types
// ABOUTME: Defines Activity, SportType, SegmentEffort, and query parameter types
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

// ============================================================================
// Sport Type
// ============================================================================

/// Enumeration of supported sport/activity types
///
/// Mirrors the `SportType` enum from dravr-platform `pierre-core`.
/// The `Other` variant handles provider-specific activity types that don't
/// map to the standard categories.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SportType {
    Run,
    Ride,
    Swim,
    Walk,
    Hike,
    VirtualRide,
    VirtualRun,
    Workout,
    Yoga,
    EbikeRide,
    MountainBike,
    GravelRide,
    CrossCountrySkiing,
    AlpineSkiing,
    Snowboarding,
    Snowshoe,
    IceSkating,
    BackcountrySkiing,
    Kayaking,
    Canoeing,
    Rowing,
    Paddleboarding,
    Surfing,
    Kitesurfing,
    StrengthTraining,
    Crossfit,
    Pilates,
    RockClimbing,
    TrailRunning,
    Soccer,
    Basketball,
    Tennis,
    Golf,
    Skateboarding,
    InlineSkating,
    Other(String),
}

impl SportType {
    /// Parse a Strava activity type string into a `SportType`.
    /// Handles both API identifiers and display names (with or without spaces)
    /// as well as French labels ("Course à pied", "Ski de fond").
    #[must_use]
    pub fn from_strava(strava_type: &str) -> Self {
        match strava_type.trim() {
            // API identifiers (camelCase)
            "Run" | "Course à pied" => Self::Run,
            "Ride" | "Sortie à vélo" | "Vélo" => Self::Ride,
            "Swim" | "Natation" => Self::Swim,
            "Walk" | "Marche" | "Marche à pied" => Self::Walk,
            "Hike" | "Randonnée" => Self::Hike,
            "VirtualRide" | "Virtual Ride" => Self::VirtualRide,
            "VirtualRun" | "Virtual Run" => Self::VirtualRun,
            "Workout" | "Exercice" => Self::Workout,
            "Yoga" => Self::Yoga,
            "EBikeRide" | "E-Bike Ride" => Self::EbikeRide,
            "MountainBikeRide" | "Mountain Bike Ride" => Self::MountainBike,
            "GravelRide" | "Gravel Ride" => Self::GravelRide,
            "CrossCountrySkiing"
            | "Cross-Country Skiing"
            | "Nordic Ski"
            | "Ski de fond"
            | "Ski de fond classique"
            | "Ski de fond skating" => Self::CrossCountrySkiing,
            "AlpineSkiing" | "AlpineSki" | "Alpine Ski" | "Ski alpin" => Self::AlpineSkiing,
            "Snowboarding" | "Snowboard" => Self::Snowboarding,
            "Snowshoe" | "Raquette" => Self::Snowshoe,
            "IceSkate" | "Ice Skate" | "Ice Skating" | "Patin à glace" => Self::IceSkating,
            "BackcountrySki" | "Backcountry Ski" | "Ski de randonnée" => Self::BackcountrySkiing,
            "Kayaking" | "Kayak" => Self::Kayaking,
            "Canoeing" | "Canot" | "Canoë" => Self::Canoeing,
            "Rowing" | "Aviron" => Self::Rowing,
            "StandUpPaddling" | "Stand Up Paddling" => Self::Paddleboarding,
            "Surfing" | "Surf" => Self::Surfing,
            "Kitesurf" | "Kitesurfing" => Self::Kitesurfing,
            "WeightTraining" | "Weight Training" | "Musculation" => Self::StrengthTraining,
            "Crossfit" | "CrossFit" => Self::Crossfit,
            "Pilates" => Self::Pilates,
            "RockClimbing" | "Rock Climbing" | "Escalade" => Self::RockClimbing,
            "TrailRun" | "Trail Run" | "Trail" | "Course de trail" => Self::TrailRunning,
            "Soccer" | "Football" => Self::Soccer,
            "Basketball" | "Basket" => Self::Basketball,
            "Tennis" => Self::Tennis,
            "Golf" => Self::Golf,
            "Skateboard" | "Skateboarding" => Self::Skateboarding,
            "InlineSkate" | "Inline Skate" | "Inline Skating" | "Roller" => Self::InlineSkating,
            other => Self::Other(other.to_owned()),
        }
    }

    /// Human-readable display name
    #[must_use]
    pub const fn display_name(&self) -> &'static str {
        match self {
            Self::Run => "Run",
            Self::Ride => "Ride",
            Self::Swim => "Swim",
            Self::Walk => "Walk",
            Self::Hike => "Hike",
            Self::VirtualRide => "Virtual Ride",
            Self::VirtualRun => "Virtual Run",
            Self::Workout => "Workout",
            Self::Yoga => "Yoga",
            Self::EbikeRide => "E-Bike Ride",
            Self::MountainBike => "Mountain Bike",
            Self::GravelRide => "Gravel Ride",
            Self::CrossCountrySkiing => "Cross-Country Skiing",
            Self::AlpineSkiing => "Alpine Skiing",
            Self::Snowboarding => "Snowboarding",
            Self::Snowshoe => "Snowshoe",
            Self::IceSkating => "Ice Skating",
            Self::BackcountrySkiing => "Backcountry Skiing",
            Self::Kayaking => "Kayaking",
            Self::Canoeing => "Canoeing",
            Self::Rowing => "Rowing",
            Self::Paddleboarding => "Paddleboarding",
            Self::Surfing => "Surfing",
            Self::Kitesurfing => "Kitesurfing",
            Self::StrengthTraining => "Strength Training",
            Self::Crossfit => "CrossFit",
            Self::Pilates => "Pilates",
            Self::RockClimbing => "Rock Climbing",
            Self::TrailRunning => "Trail Running",
            Self::Soccer => "Soccer",
            Self::Basketball => "Basketball",
            Self::Tennis => "Tennis",
            Self::Golf => "Golf",
            Self::Skateboarding => "Skateboarding",
            Self::InlineSkating => "Inline Skating",
            Self::Other(_) => "Other",
        }
    }
}

impl fmt::Display for SportType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

// ============================================================================
// Segment Effort
// ============================================================================

/// Segment effort within an activity (primarily from Strava)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SegmentEffort {
    /// Unique identifier for the segment effort
    pub id: String,
    /// Name of the segment
    pub name: String,
    /// Elapsed time on segment in seconds
    pub elapsed_time: u64,
    /// Moving time on segment in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moving_time: Option<u64>,
    /// Distance of the segment in meters
    pub distance: f64,
    /// Average heart rate during segment (BPM)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_heart_rate: Option<u32>,
    /// Max heart rate during segment (BPM)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_heart_rate: Option<u32>,
    /// Average cadence during segment
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_cadence: Option<u32>,
    /// Average power during segment (watts)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_watts: Option<u32>,
}

// ============================================================================
// Activity
// ============================================================================

/// A single fitness activity scraped from Strava's training page
///
/// Mirrors the `Activity` struct from dravr-platform `pierre-core`.
/// All optional fields use `Option` to handle partial data from scraping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    /// Unique Strava activity identifier
    pub id: String,
    /// Human-readable activity name/title
    pub name: String,
    /// Type of sport/activity
    pub sport_type: SportType,
    /// When the activity started (UTC)
    pub start_date: DateTime<Utc>,
    /// Total duration in seconds
    pub duration_seconds: u64,

    // Basic metrics
    /// Total distance in meters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_meters: Option<f64>,
    /// Total elevation gain in meters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elevation_gain: Option<f64>,
    /// Average heart rate (BPM)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_heart_rate: Option<u32>,
    /// Max heart rate (BPM)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_heart_rate: Option<u32>,
    /// Average speed (m/s)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_speed: Option<f64>,
    /// Max speed (m/s)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_speed: Option<f64>,
    /// Estimated calories burned
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calories: Option<u32>,

    // Power metrics
    /// Average power (watts)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_power: Option<u32>,
    /// Max power (watts)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_power: Option<u32>,
    /// Normalized power (watts)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normalized_power: Option<u32>,

    // Cadence
    /// Average cadence (RPM or steps/min)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_cadence: Option<u32>,

    // Training load
    /// Training Stress Score
    #[serde(skip_serializing_if = "Option::is_none")]
    pub training_stress_score: Option<f32>,
    /// Intensity Factor
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intensity_factor: Option<f32>,
    /// Strava suffer score / relative effort
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suffer_score: Option<u32>,

    // Location
    /// Starting latitude
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_latitude: Option<f64>,
    /// Starting longitude
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_longitude: Option<f64>,
    /// City where the activity took place
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// Region/state/province
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Country
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,

    // Environmental conditions
    /// Temperature during activity (Celsius)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Feels-like temperature (Celsius)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feels_like: Option<f32>,
    /// Humidity percentage
    #[serde(skip_serializing_if = "Option::is_none")]
    pub humidity: Option<f32>,
    /// Wind speed (km/h)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_speed: Option<f32>,
    /// Wind direction (e.g., "NW", "SE")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_direction: Option<String>,
    /// Weather condition (e.g., "Clear", "Cloudy")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weather: Option<String>,

    // Pace
    /// Average pace (e.g., "6:55 /km")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pace: Option<String>,
    /// Grade Adjusted Pace (e.g., "6:25 /km")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap: Option<String>,

    // Timing
    /// Elapsed time in seconds (includes stopped time)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_time_seconds: Option<u64>,

    // Device and gear
    /// Recording device (e.g., "Garmin fēnix 6S Pro")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    /// Gear used (e.g., "Salomon Spikecross (224.2 km)")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gear_name: Option<String>,

    // Perceived exertion
    /// Perceived exertion level (e.g., "Moderate", "Hard")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub perceived_exertion: Option<String>,

    // Classification
    /// Workout type (0=default, 1=race, 2=long run, 3=workout)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workout_type: Option<u32>,
    /// Detailed sport type from Strava (e.g., "Trail Run", "Nordic Ski")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sport_type_detail: Option<String>,

    // Segments
    /// Segment efforts within this activity
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_efforts: Option<Vec<SegmentEffort>>,

    /// Source provider (always "strava-scraper")
    pub provider: String,
}

// ============================================================================
// Athlete Profile
// ============================================================================

/// Profile data for the authenticated athlete
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AthleteProfile {
    /// Full display name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// First name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firstname: Option<String>,
    /// Last name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lastname: Option<String>,
    /// URL to the profile picture
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_picture_url: Option<String>,
    /// City
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// Country
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
}

// ============================================================================
// Query Parameters
// ============================================================================

/// Parameters for filtering activity queries
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActivityParams {
    /// Maximum number of activities to return
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Only return activities before this date
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<DateTime<Utc>>,
    /// Only return activities after this date
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<DateTime<Utc>>,
    /// Filter by sport type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sport_type: Option<String>,
    /// Navigate into each activity detail page for full metrics (slower but richer data)
    #[serde(default)]
    pub enrich_details: bool,
}

// ============================================================================
// Daily Health Summary
// ============================================================================

/// Daily health and wellness summary scraped from a provider's dashboard page.
///
/// All metric fields are optional — providers fill what they can.
/// The same struct is used across all providers (Garmin, Strava, etc.),
/// each populating different fields based on available data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailySummary {
    /// Calendar date for this summary
    pub date: NaiveDate,
    /// Source provider name (e.g., "garmin", "strava")
    pub provider: String,

    // Heart rate
    /// Resting heart rate (BPM)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resting_heart_rate: Option<u32>,
    /// 7-day average resting heart rate (BPM)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_resting_heart_rate_7day: Option<u32>,
    /// Highest heart rate of the day (BPM)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_heart_rate: Option<u32>,

    // Body battery (Garmin-specific, 0-100 scale)
    /// Current body battery level
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_battery: Option<u32>,

    // Stress (0-100 scale)
    /// Average stress level
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stress_level: Option<u32>,

    // Steps
    /// Total steps for the day
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    /// Daily step goal
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_goal: Option<u32>,

    // Intensity minutes
    /// Weekly intensity minutes accumulated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intensity_minutes: Option<u32>,
    /// Weekly intensity minutes goal
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intensity_minutes_goal: Option<u32>,

    // Training status
    /// VO2 max estimate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vo2_max: Option<f32>,
    /// Training load (7-day cumulative)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub training_load: Option<u32>,

    // Sleep
    /// Sleep quality score (0-100)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sleep_score: Option<u32>,
    /// Total sleep duration in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sleep_duration_seconds: Option<u64>,
    /// Deep sleep duration in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sleep_deep_seconds: Option<u64>,
    /// Light sleep duration in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sleep_light_seconds: Option<u64>,
    /// REM sleep duration in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sleep_rem_seconds: Option<u64>,
    /// Awake duration in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sleep_awake_seconds: Option<u64>,

    // HRV (Heart Rate Variability)
    /// HRV status label (e.g., "Balanced", "Low", "Unbalanced")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hrv_status: Option<String>,
    /// HRV value in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hrv_value: Option<u32>,

    // Body composition
    /// Body weight in kilograms
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_kg: Option<f32>,
    /// Body fat percentage
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_fat_percent: Option<f32>,

    // Training load (Strava Fitness & Freshness)
    /// Functional Threshold Power (watts)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ftp: Option<u32>,
    /// Fitness score (CTL — Chronic Training Load)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fitness_score: Option<u32>,
    /// Fatigue score (ATL — Acute Training Load)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fatigue_score: Option<u32>,
    /// Form score (TSB — Training Stress Balance)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub form_score: Option<i32>,

    // Calories
    /// Active calories burned
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_calories: Option<u32>,
    /// Total calories (active + resting)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_calories: Option<u32>,
}

// ============================================================================
// Health Query Parameters
// ============================================================================

/// Parameters for daily health summary queries
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthParams {
    /// Calendar date to retrieve health data for
    pub date: NaiveDate,
}

// ============================================================================
// Auth Session
// ============================================================================

/// Represents an authenticated Strava session with browser cookies
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSession {
    /// Session identifier (for cache keying)
    pub session_id: String,
    /// Serialized browser cookies (encrypted at rest)
    pub cookies: Vec<CookieData>,
    /// When this session was created
    pub created_at: DateTime<Utc>,
    /// When this session expires (if known)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// A single browser cookie
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookieData {
    /// Cookie name
    pub name: String,
    /// Cookie value
    pub value: String,
    /// Cookie domain
    pub domain: String,
    /// Cookie path
    pub path: String,
    /// Whether cookie is secure-only
    pub secure: bool,
    /// Whether cookie is HTTP-only
    pub http_only: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sport_type_from_strava() {
        assert_eq!(SportType::from_strava("Run"), SportType::Run);
        assert_eq!(SportType::from_strava("Ride"), SportType::Ride);
        assert_eq!(
            SportType::from_strava("MountainBikeRide"),
            SportType::MountainBike
        );
        assert_eq!(
            SportType::from_strava("WeightTraining"),
            SportType::StrengthTraining
        );
        assert_eq!(
            SportType::from_strava("Unknown"),
            SportType::Other("Unknown".to_owned())
        );
    }

    #[test]
    fn sport_type_display() {
        assert_eq!(SportType::Run.display_name(), "Run");
        assert_eq!(SportType::MountainBike.display_name(), "Mountain Bike");
        assert_eq!(SportType::Other("Foo".to_owned()).display_name(), "Other");
    }

    #[test]
    fn daily_summary_serialization() {
        let summary = DailySummary {
            date: NaiveDate::from_ymd_opt(2026, 3, 30).unwrap(), // Safe: valid date literal
            provider: "garmin".to_owned(),
            resting_heart_rate: Some(49),
            average_resting_heart_rate_7day: Some(52),
            max_heart_rate: Some(113),
            body_battery: Some(75),
            stress_level: Some(19),
            steps: Some(5156),
            step_goal: None,
            intensity_minutes: Some(72),
            intensity_minutes_goal: None,
            vo2_max: Some(50.0),
            training_load: Some(326),
            sleep_score: None,
            sleep_duration_seconds: None,
            sleep_deep_seconds: None,
            sleep_light_seconds: None,
            sleep_rem_seconds: None,
            sleep_awake_seconds: None,
            hrv_status: None,
            hrv_value: None,
            weight_kg: None,
            body_fat_percent: None,
            ftp: None,
            fitness_score: None,
            fatigue_score: None,
            form_score: None,
            active_calories: None,
            total_calories: None,
        };
        let json = serde_json::to_string(&summary).expect("serialize"); // Safe: test with serializable struct
        assert!(json.contains("2026-03-30"));
        assert!(json.contains(r#""resting_heart_rate":49"#));
        assert!(!json.contains("sleep_score")); // None fields skipped
        assert!(!json.contains("step_goal"));
    }

    #[test]
    fn health_params_date() {
        let params = HealthParams {
            date: NaiveDate::from_ymd_opt(2026, 3, 30).unwrap(), // Safe: valid date literal
        };
        let json = serde_json::to_string(&params).expect("serialize"); // Safe: test with serializable struct
        assert!(json.contains("2026-03-30"));
    }

    #[test]
    fn activity_serialization() {
        let activity = Activity {
            id: "123".to_owned(),
            name: "Morning Run".to_owned(),
            sport_type: SportType::Run,
            start_date: Utc::now(),
            duration_seconds: 1800,
            distance_meters: Some(5000.0),
            elevation_gain: None,
            average_heart_rate: Some(150),
            max_heart_rate: None,
            average_speed: None,
            max_speed: None,
            calories: None,
            average_power: None,
            max_power: None,
            normalized_power: None,
            average_cadence: None,
            training_stress_score: None,
            intensity_factor: None,
            suffer_score: None,
            start_latitude: None,
            start_longitude: None,
            city: None,
            region: None,
            country: None,
            temperature: None,
            feels_like: None,
            humidity: None,
            wind_speed: None,
            wind_direction: None,
            weather: None,
            pace: None,
            gap: None,
            elapsed_time_seconds: None,
            device_name: None,
            gear_name: None,
            perceived_exertion: None,
            workout_type: None,
            sport_type_detail: None,
            segment_efforts: None,
            provider: "strava-scraper".to_owned(),
        };
        let json = serde_json::to_string(&activity).expect("serialize"); // Safe: test with serializable struct
        assert!(json.contains("Morning Run"));
        assert!(!json.contains("elevation_gain")); // None fields skipped
    }
}
