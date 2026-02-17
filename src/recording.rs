use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::app::{PodMemorySnapshot, ProcessSnapshot};

const MAGIC: &[u8; 4] = b"SPMR";
const VERSION: u8 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordingSnapshot {
    pub timestamp: u64,
    pub processes: Vec<ProcessSnapshot>,
    pub pod_memory: PodMemorySnapshot,
    pub cpu_cores: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RecordingMetadata {
    pub id: String,
    pub start_time: u64,
    pub end_time: u64,
    pub trigger_pid: u32,
    pub trigger_name: String,
    pub snapshot_count: usize,
    pub file_path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recording {
    pub metadata: RecordingMetadata,
    pub snapshots: Vec<RecordingSnapshot>,
}

#[derive(Clone, Debug)]
pub struct RecordingManager {
    buffer: VecDeque<RecordingSnapshot>,
    max_snapshots: usize,
    recordings_dir: PathBuf,
    last_saved_pids: HashMap<u32, Instant>,
}

impl RecordingManager {
    pub fn new() -> Self {
        let max_snapshots = env::var("SPM_RECORDING_WINDOW")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(300);
        let recordings_dir = Self::ensure_recordings_dir();
        let manager = Self {
            buffer: VecDeque::with_capacity(max_snapshots),
            max_snapshots,
            recordings_dir,
            last_saved_pids: HashMap::new(),
        };
        manager.cleanup_old_recordings();
        manager
    }

    pub fn add_snapshot(&mut self, snapshot: RecordingSnapshot) {
        self.buffer.push_back(snapshot);
        while self.buffer.len() > self.max_snapshots {
            self.buffer.pop_front();
        }
    }

    pub fn save_recording(&mut self, trigger_pid: u32, trigger_name: String) -> Option<usize> {
        if self.buffer.is_empty() {
            return None;
        }

        let now = Instant::now();
        if let Some(last_saved) = self.last_saved_pids.get(&trigger_pid) {
            if now.duration_since(*last_saved) < Duration::from_secs(2) {
                return None;
            }
        }

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let id = format!("recording_{}_{}", timestamp, trigger_pid);
        let file_path = self.recordings_dir.join(format!("{}.bin", id));
        let start_time = self
            .buffer
            .front()
            .map(|snapshot| snapshot.timestamp)
            .unwrap_or(timestamp);
        let end_time = self
            .buffer
            .back()
            .map(|snapshot| snapshot.timestamp)
            .unwrap_or(timestamp);
        let metadata = RecordingMetadata {
            id,
            start_time,
            end_time,
            trigger_pid,
            trigger_name,
            snapshot_count: self.buffer.len(),
            file_path: file_path.clone(),
        };
        let recording = Recording {
            metadata,
            snapshots: self.buffer.iter().cloned().collect(),
        };

        let mut file = fs::File::create(&file_path).ok()?;
        file.write_all(MAGIC).ok()?;
        file.write_all(&[VERSION]).ok()?;
        let encoded = bincode::serialize(&recording).ok()?;
        file.write_all(&encoded).ok()?;
        file.flush().ok()?;

        self.last_saved_pids.insert(trigger_pid, now);
        Some(recording.snapshots.len())
    }

    pub fn list_recordings(&self) -> Vec<RecordingMetadata> {
        let entries = match fs::read_dir(&self.recordings_dir) {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };

        let mut recordings = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("bin") {
                continue;
            }
            if let Ok(recording) = Self::read_recording(&path) {
                recordings.push(recording.metadata);
            }
        }

        recordings.sort_by(|left, right| right.end_time.cmp(&left.end_time));
        recordings
    }

    pub fn load_recording(&self, id: &str) -> io::Result<Recording> {
        let path = self.recordings_dir.join(format!("{}.bin", id));
        Self::read_recording(&path)
    }

    pub fn delete_recording(&self, id: &str) -> io::Result<()> {
        let path = self.recordings_dir.join(format!("{}.bin", id));
        fs::remove_file(path)
    }

    pub fn snapshot_count(&self) -> usize {
        self.buffer.len()
    }

    pub fn max_snapshots(&self) -> usize {
        self.max_snapshots
    }

    fn cleanup_old_recordings(&self) {
        let max_age_days = env::var("SPM_RECORDING_MAX_AGE_DAYS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(7);
        let max_age = Duration::from_secs(max_age_days.saturating_mul(24 * 60 * 60));
        let now = SystemTime::now();

        let entries = match fs::read_dir(&self.recordings_dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("bin") {
                continue;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            let modified = match metadata.modified() {
                Ok(modified) => modified,
                Err(_) => continue,
            };
            let age = match now.duration_since(modified) {
                Ok(age) => age,
                Err(_) => continue,
            };
            if age > max_age {
                let _ = fs::remove_file(path);
            }
        }
    }

    fn ensure_recordings_dir() -> PathBuf {
        let recordings_dir = env::var("SPM_RECORDINGS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
                base.join(".session-process-monitor").join("recordings")
            });

        let _ = fs::create_dir_all(&recordings_dir);
        recordings_dir
    }

    fn read_recording(path: &Path) -> io::Result<Recording> {
        let mut file = fs::File::open(path)?;
        let mut header = [0u8; 5];
        file.read_exact(&mut header)?;
        if &header[0..4] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid recording magic",
            ));
        }
        if header[4] != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported recording version",
            ));
        }

        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        let mut recording: Recording = bincode::deserialize(&data)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        recording.metadata.file_path = path.to_path_buf();
        Ok(recording)
    }
}
