use std::time::Instant;

use crate::recording::{Recording, RecordingMetadata};

#[derive(Clone, Debug, PartialEq)]
pub enum AppMode {
    Live,
    RecordingList(RecordingListState),
    Replay(ReplayState),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordingListState {
    pub recordings: Vec<RecordingMetadata>,
    pub selected: usize,
}

#[derive(Clone, Debug)]
pub struct ReplayState {
    pub recording: Recording,
    pub current_index: usize,
    pub speed: PlaybackSpeed,
    pub playing: bool,
    pub last_advance_time: Instant,
}

impl PartialEq for ReplayState {
    fn eq(&self, other: &Self) -> bool {
        self.current_index == other.current_index
            && self.speed == other.speed
            && self.playing == other.playing
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlaybackSpeed {
    Half,
    Normal,
    Double,
    Fast,
    VeryFast,
}

impl PlaybackSpeed {
    pub fn interval_ms(&self) -> u64 {
        match self {
            Self::Half => 2000,
            Self::Normal => 1000,
            Self::Double => 500,
            Self::Fast => 200,
            Self::VeryFast => 100,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Half => "0.5x",
            Self::Normal => "1x",
            Self::Double => "2x",
            Self::Fast => "5x",
            Self::VeryFast => "10x",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Half => Self::Normal,
            Self::Normal => Self::Double,
            Self::Double => Self::Fast,
            Self::Fast => Self::VeryFast,
            Self::VeryFast => Self::VeryFast,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::Half => Self::Half,
            Self::Normal => Self::Half,
            Self::Double => Self::Normal,
            Self::Fast => Self::Double,
            Self::VeryFast => Self::Fast,
        }
    }
}
