//! SRT import/export. Timecodes are kept in a sidecar and never guessed.
use super::{Result, Translations, invalid};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    sync::atomic::{AtomicU64, Ordering},
};
use ziyuan_storage::EventLog;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Cue {
    index: u32,
    start: String,
    end: String,
    text: String,
}

fn parse_srt(text: &str) -> Result<Vec<Cue>> {
    let mut cues = Vec::new();
    for block in text.replace("\r\n", "\n").split("\n\n") {
        let mut lines = block.lines().filter(|l| !l.trim().is_empty());
        let Some(first) = lines.next() else { continue };
        let index = first
            .trim()
            .parse::<u32>()
            .map_err(|_| invalid("SRT 字幕序号无效"))?;
        if index == 0 || cues.iter().any(|c: &Cue| c.index == index) {
            return Err(invalid("SRT 字幕序号为零或重复"));
        }
        let timing = lines.next().ok_or_else(|| invalid("SRT 缺少时间轴"))?;
        let Some((start, end)) = timing.split_once("-->") else {
            return Err(invalid(format!("SRT 第 {index} 条时间轴格式不对")));
        };
        let body = lines.collect::<Vec<_>>().join("\n");
        if body.trim().is_empty() {
            return Err(invalid(format!("SRT 第 {index} 条没有文字")));
        }
        if timecode(start.trim())? >= timecode(end.trim())? {
            return Err(invalid("SRT 结束时间必须晚于开始时间"));
        }
        cues.push(Cue {
            index,
            start: start.trim().to_owned(),
            end: end.trim().to_owned(),
            text: body,
        });
    }
    if cues.is_empty() {
        return Err(invalid("SRT 没有可导入的字幕"));
    }
    Ok(cues)
}
fn timecode(text: &str) -> Result<u64> {
    let parts = text.split([':', ',']).collect::<Vec<_>>();
    if parts.len() != 4
        || parts[0].len() < 2
        || parts[1].len() != 2
        || parts[2].len() != 2
        || parts[3].len() != 3
        || parts.iter().any(|p| !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(invalid("SRT 时间码须为 HH:MM:SS,mmm"));
    }
    let values = parts
        .iter()
        .map(|p| p.parse::<u64>().map_err(|_| invalid("SRT 时间码过大")))
        .collect::<Result<Vec<_>>>()?;
    if values[0] > 9999 || values[1] >= 60 || values[2] >= 60 {
        return Err(invalid("SRT 时间码超出范围"));
    }
    Ok(((values[0] * 60 + values[1]) * 60 + values[2]) * 1000 + values[3])
}

impl Translations {
    fn srt_path(&self, project: &str) -> std::path::PathBuf {
        self.root
            .join(".ziyuan")
            .join("text-projects")
            .join(project)
            .join("srt.json")
    }

    pub fn import_srt(&self, project: &str, path: &std::path::Path) -> Result<usize> {
        let _guard = self.lock_local_translation(project)?;
        if !self.project(project)?.chapters.is_empty() {
            return Err(invalid("已有原文，拒绝覆盖SRT"));
        }
        let bytes = self
            .read_local_translation_file(path)?
            .ok_or_else(|| invalid("SRT 文件不存在"))?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(invalid("SRT 超过8 MiB"));
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| invalid("SRT 须为UTF-8"))?;
        let cues = parse_srt(text)?;
        let body = cues
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temp = self.store_dir(project)?.join(format!(
            ".srt-source-{}-{}.txt",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all((body + "\n").as_bytes())?;
        file.sync_all()?;
        drop(file);
        // Publish the timeline before the source manifest so a successful import
        // never becomes visible without its timecodes. Failed staging is retryable.
        super::write_plain(&self.srt_path(project), &serde_json::to_vec_pretty(&cues)?)?;
        let result = self
            .texts()
            .import(project, std::slice::from_ref(&temp))
            .map_err(|e| invalid(e.to_string()));
        let _ = std::fs::remove_file(&temp);
        result?;
        Ok(cues.len())
    }

    pub fn export_srt(&self, project: &str, path: &std::path::Path) -> Result<usize> {
        let _guard = self.lock_local_translation(project)?;
        let mut session =
            crate::Session::open(EventLog::new(self.root())).map_err(|e| invalid(e.to_string()))?;
        session
            .export_finished(self.root(), project, &Default::default())
            .map_err(|e| invalid(e.to_string()))?;
        let cues: Vec<Cue> = serde_json::from_slice(
            &self
                .read_local_translation_file(&self.srt_path(project))?
                .ok_or_else(|| invalid("srt.json 不存在"))?,
        )
        .map_err(|e| invalid(format!("srt.json 格式不对：{e}")))?;
        let project_file = self.project(project)?;
        let mut targets = Vec::new();
        for chapter in &project_file.chapters {
            for block in &chapter.blocks {
                let bytes = self
                    .read_translation(project, &block.block_id)?
                    .ok_or_else(|| invalid("SRT 译稿缺块"))?;
                let target = super::read_approved_target(&bytes, &block.block_id)?;
                let paragraphs = match target {
                    super::ApprovedTarget::Plain(target) => {
                        if target.paragraphs.iter().any(|p| !p.inlines.is_empty()) {
                            return Err(invalid("SRT 无法无损表达结构化文学标记，请使用HTML成稿"));
                        }
                        target
                            .paragraphs
                            .into_iter()
                            .map(|p| (p.id, p.text))
                            .collect::<Vec<_>>()
                    }
                    super::ApprovedTarget::Marked(target) => {
                        if target.paragraphs.iter().any(|p| !p.marks.is_empty()) {
                            return Err(invalid("SRT 无法无损表达文学标记，请使用HTML成稿"));
                        }
                        target
                            .paragraphs
                            .into_iter()
                            .map(|p| (p.id, p.text))
                            .collect()
                    }
                };
                if paragraphs.len() != block.paragraphs.len() {
                    return Err(invalid("SRT 译稿段数不符"));
                }
                for ((id, text), source) in paragraphs.into_iter().zip(&block.paragraphs) {
                    if id != source.id || text.trim().is_empty() {
                        return Err(invalid("SRT 译稿段号不符或为空"));
                    }
                    targets.push(text);
                }
            }
        }
        if targets.len() != cues.len() {
            return Err(invalid(format!(
                "译稿段数 {} 与 SRT 字幕数 {} 不一致",
                targets.len(),
                cues.len()
            )));
        }
        let mut out = String::new();
        for (i, (cue, target)) in cues.iter().zip(targets).enumerate() {
            if timecode(&cue.start)? >= timecode(&cue.end)? {
                return Err(invalid("SRT 时间轴损坏"));
            }
            out.push_str(&format!(
                "{}\n{} --> {}\n{}\n\n",
                i + 1,
                cue.start,
                cue.end,
                target
            ));
        }
        let history = session
            .history()
            .map_err(|e| invalid(e.to_string()))?
            .to_vec();
        EventLog::new(self.root())
            .append_if_unchanged_then(&history, &[], || {
                super::backup::publish_backup(self.root(), path, out.as_bytes())
            })
            .map_err(|e| invalid(e.to_string()))??;
        Ok(cues.len())
    }
}
