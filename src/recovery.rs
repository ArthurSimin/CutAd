//! Local crash recovery for unsaved timeline changes.

use anyhow::{Context, Result};
use cutad::project::{self, Project};
use eframe::egui;
use std::fs::{self, File, OpenOptions};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

const INTERVAL: Duration = Duration::from_millis(1500);

#[derive(Default)]
pub struct Autosave {
    draft: Option<Draft>,
    deadline: Option<Instant>,
    writing: Option<JoinHandle<Result<()>>>,
}

struct Draft {
    path: PathBuf,
    locked: File,
}

pub struct Candidate {
    path: PathBuf,
    pub name: String,
    pub modified: SystemTime,
}

impl Autosave {
    pub fn mark_dirty(&mut self) {
        self.deadline
            .get_or_insert_with(|| Instant::now() + INTERVAL);
    }

    pub fn tick(&mut self, ctx: &egui::Context, project: &Project) -> Result<bool> {
        let mut saved = false;
        if self.writing.as_ref().is_some_and(|task| task.is_finished()) {
            self.writing
                .take()
                .expect("autosave worker")
                .join()
                .map_err(|_| anyhow::anyhow!("The autosave worker stopped."))??;
            saved = true;
        }
        if let Some(due) = self.deadline {
            if due > Instant::now() {
                ctx.request_repaint_after(due.saturating_duration_since(Instant::now()));
            } else if self.writing.is_none() {
                self.deadline = None;
                if self.draft.is_none() {
                    self.draft = Some(Draft::new(&state_root()?.join("Recovery"))?);
                }
                let path = self
                    .draft
                    .as_ref()
                    .expect("active recovery file")
                    .path
                    .clone();
                let project = project.clone();
                let ctx = ctx.clone();
                self.writing = Some(std::thread::spawn(move || {
                    let result = project::save(&project, &path);
                    ctx.request_repaint();
                    result
                }));
            }
        }
        Ok(saved)
    }

    pub fn clear(&mut self) {
        self.deadline = None;
        if let Some(draft) = self.draft.take() {
            if let Some(worker) = self.writing.take() {
                let _ = worker.join();
            }
            draft.remove();
        }
    }

    pub fn recover(candidate: &Candidate) -> Result<(Self, Project)> {
        let draft = Draft::claim(candidate.path.clone())?;
        let project = project::load(&draft.path)?;
        Ok((
            Self {
                draft: Some(draft),
                deadline: None,
                writing: None,
            },
            project,
        ))
    }
}

impl Drop for Autosave {
    fn drop(&mut self) {
        // Let pending writes finish with the draft still locked if the editor exits unexpectedly.
        if let Some(worker) = self.writing.take() {
            let _ = worker.join();
        }
    }
}

impl Draft {
    fn new(folder: &Path) -> Result<Self> {
        fs::create_dir_all(folder)?;
        Self::claim(folder.join(project::new_id()).with_extension("cutad"))
    }

    fn claim(path: PathBuf) -> Result<Self> {
        let locked = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))
            .context("Could not open the recovery lock.")?;
        locked
            .try_lock()
            .context("This recovery draft is open in another CutAd window.")?;
        Ok(Self { path, locked })
    }

    fn remove(self) {
        let _ = fs::remove_file(&self.path);
        drop(self.locked);
        let _ = fs::remove_file(self.path.with_extension("lock"));
    }
}

pub fn candidates() -> Result<Vec<Candidate>> {
    let folder = state_root()?.join("Recovery");
    if !folder.exists() {
        return Ok(Vec::new());
    }
    scan(&folder)
}

pub fn recent_projects() -> Vec<PathBuf> {
    state_root().map_or_else(|_| Vec::new(), |folder| read_recent(&folder))
}

pub fn remember_project(path: &Path) -> Result<Vec<PathBuf>> {
    remember_in(&state_root()?, &std::path::absolute(path)?)
}

fn remember_in(folder: &Path, path: &Path) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(folder)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(folder.join("recent.lock"))?;
    lock.lock()?;
    let mut paths = read_recent(folder);
    paths.retain(|other| !project::same_path(other, path));
    paths.insert(0, path.to_owned());
    paths.truncate(12);
    let temp = tempfile::NamedTempFile::new_in(folder)?;
    serde_json::to_writer(temp.as_file(), &paths)?;
    temp.as_file().sync_all()?;
    temp.persist(folder.join("recent.json"))?;
    Ok(paths)
}

fn read_recent(folder: &Path) -> Vec<PathBuf> {
    let mut entries = File::open(folder.join("recent.json"))
        .ok()
        .and_then(|file| serde_json::from_reader::<_, Vec<PathBuf>>(BufReader::new(file)).ok())
        .unwrap_or_default();
    entries.retain(|path| path.is_absolute() && path.is_file());
    entries.truncate(12);
    entries
}

fn scan(folder: &Path) -> Result<Vec<Candidate>> {
    let mut candidates = Vec::new();
    for item in fs::read_dir(folder)? {
        let item = item?;
        let path = item.path();
        if path.extension().is_none_or(|ext| ext != "cutad") || !item.file_type()?.is_file() {
            continue;
        }
        if let Ok(draft) = Draft::claim(path.clone())
            && let Ok(project) = project::load(&draft.path)
        {
            candidates.push(Candidate {
                path,
                name: project.name,
                modified: item.metadata()?.modified()?,
            });
        }
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.modified));
    Ok(candidates)
}

fn state_root() -> Result<PathBuf> {
    let absolute_env = |name| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let base = if cfg!(windows) {
        absolute_env("LOCALAPPDATA")
    } else {
        absolute_env("XDG_STATE_HOME")
            .or_else(|| absolute_env("HOME").map(|home| home.join(".local/state")))
    };
    Ok(base
        .context("Could not locate the user state directory.")?
        .join("CutAd"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_excludes_active_instances_and_keeps_recovered_media_paths() -> Result<()> {
        use cutad::project::Asset;
        let folder = tempfile::tempdir()?;
        let draft = Draft::new(folder.path())?;
        let media = folder.path().join("test video.mkv");
        let project = Project {
            name: "Unsaved edit".into(),
            assets: vec![Asset::video(&media, 30, false)],
            ..Project::default()
        };
        project::save(&project, &draft.path)?;
        assert!(scan(folder.path())?.is_empty());
        drop(draft);
        let candidates = scan(folder.path())?;
        assert_eq!(candidates.len(), 1);
        let (mut session, recovered) = Autosave::recover(&candidates[0])?;
        assert_eq!(recovered, project);
        assert!(scan(folder.path())?.is_empty());
        session.clear();
        assert!(scan(folder.path())?.is_empty());
        Ok(())
    }

    #[test]
    fn recent_projects_reorder_and_skip_missing_files() -> Result<()> {
        let root = tempfile::tempdir()?;
        let first = root.path().join("First.cutad");
        let second = root.path().join("Second.cutad");
        project::save(&Project::default(), &first)?;
        project::save(&Project::default(), &second)?;
        remember_in(root.path(), &first)?;
        remember_in(root.path(), &second)?;
        assert_eq!(read_recent(root.path()), [second.clone(), first.clone()]);
        remember_in(root.path(), &first)?;
        assert_eq!(read_recent(root.path()), [first.clone(), second.clone()]);
        fs::remove_file(&second)?;
        assert_eq!(read_recent(root.path()), [first]);
        Ok(())
    }
}
