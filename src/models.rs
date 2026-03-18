// ABOUTME: Activity data models mirroring dravr-platform pierre-core types
// ABOUTME: Defines Activity, SportType, SegmentEffort, and query parameter types
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use chrono::{DateTime, Utc};
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
    /// Parse a Strava activity type string into a `SportType`
    #[must_use]
    pub fn from_strava(strava_type: &str) -> Self {
        match strava_type {
            "Run" => Self::Run,
            "Ride" => Self::Ride,
            "Swim" => Self::Swim,
            "Walk" => Self::Walk,
            "Hike" => Self::Hike,
            "VirtualRide" => Self::VirtualRide,
            "VirtualRun" => Self::VirtualRun,
            "Workout" => Self::Workout,
            "Yoga" => Self::Yoga,
            "EBikeRide" => Self::EbikeRide,
            "MountainBikeRide" => Self::MountainBike,
            "GravelRide" => Self::GravelRide,
            "CrossCountrySkiing" => Self::CrossCountrySkiing,
            "AlpineSkiing" | "AlpineSki" => Self::AlpineSkiing,
            "Snowboarding" | "Snowboard" => Self::Snowboarding,
            "Snowshoe" => Self::Snowshoe,
            "IceSkate" => Self::IceSkating,
            "BackcountrySki" => Self::BackcountrySkiing,
            "Kayaking" => Self::Kayaking,
            "Canoeing" => Self::Canoeing,
            "Rowing" => Self::Rowing,
            "StandUpPaddling" => Self::Paddleboarding,
            "Surfing" => Self::Surfing,
            "Kitesurf" => Self::Kitesurfing,
            "WeightTraining" => Self::StrengthTraining,
            "Crossfit" => Self::Crossfit,
            "Pilates" => Self::Pilates,
            "RockClimbing" => Self::RockClimbing,
            "TrailRun" => Self::TrailRunning,
            "Soccer" => Self::Soccer,
            "Basketball" => Self::Basketball,
            "Tennis" => Self::Tennis,
            "Golf" => Self::Golf,
            "Skateboard" => Self::Skateboarding,
            "InlineSkate" => Self::InlineSkating,
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

impl std::fmt::Display for SportType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

    // Classification
    /// Workout type (0=default, 1=race, 2=long run, 3=workout)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workout_type: Option<u32>,
    /// Detailed sport type from Strava (e.g., "`MountainBikeRide`", "`TrailRun`")
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
            workout_type: None,
            sport_type_detail: None,
            segment_efforts: None,
            provider: "strava-scraper".to_owned(),
        };
        let json = serde_json::to_string(&activity).expect("serialize");
        assert!(json.contains("Morning Run"));
        assert!(!json.contains("elevation_gain")); // None fields skipped
    }
}
