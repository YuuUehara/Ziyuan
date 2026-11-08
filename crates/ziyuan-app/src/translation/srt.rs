//! SRT import/export. Timecodes are kept in a sidecar and never guessed.
use super::{Result, Translations, invalid};
use serde::{Deserialize, Serialize};

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
        let Some(index) = lines.next().and_then(|s| s.trim().parse::<u32>().ok()) else {
            continue;
        };
        let Some(timing) = lines.next() else { continue };
        let Some((start, end)) = timing.split_once("-->") else {
            return Err(invalid(format!("SRT 第 {index} 条时间轴格式不对")));
        };
        let body = lines.collect::<Vec<_>>().join("\n");
        if body.trim().is_empty() {
            return Err(invalid(format!("SRT 第 {index} 条没有文字")));
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

impl Translations {
    fn srt_path(&self, project: &str) -> std::path::PathBuf {
        self.root
            .join(".ziyuan")
            .join("text-projects")
            .join(project)
            .join("srt.json")
    }

    pub fn import_srt(&self, project: &str, path: &std::path::Path) -> Result<usize> {
        let text =
            std::fs::read_to_string(path).map_err(|e| invalid(format!("读取 SRT 失败：{e}")))?;
        let cues = parse_srt(&text)?;
        let body = cues
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let temp = std::env::temp_dir().join(format!("ziyuan-srt-{}.txt", std::process::id()));
        std::fs::write(&temp, body + "\n").map_err(|e| invalid(e.to_string()))?;
        let result = self
            .texts()
            .import(project, std::slice::from_ref(&temp))
            .map_err(|e| invalid(e.to_string()));
        let _ = std::fs::remove_file(&temp);
        result?;
        let sidecar = self.srt_path(project);
        if let Some(parent) = sidecar.parent() {
            std::fs::create_dir_all(parent).map_err(|e| invalid(e.to_string()))?;
        }
        std::fs::write(
            &sidecar,
            serde_json::to_vec_pretty(&cues).map_err(|e| invalid(e.to_string()))?,
        )
        .map_err(|e| invalid(e.to_string()))?;
        Ok(cues.len())
    }

    pub fn export_srt(&self, project: &str, path: &std::path::Path) -> Result<usize> {
        let cues: Vec<Cue> = serde_json::from_slice(
            &std::fs::read(self.srt_path(project))
                .map_err(|e| invalid(format!("读取 srt.json 失败：{e}")))?,
        )
        .map_err(|e| invalid(format!("srt.json 格式不对：{e}")))?;
        let project_file = self.project(project)?;
        let mut targets = Vec::new();
        for chapter in &project_file.chapters {
            for block in &chapter.blocks {
                let Some(bytes) = self.read_translation(project, &block.block_id)? else {
                    continue;
                };
                let Ok(target) = serde_json::from_slice::<crate::text_project::PromptBlock>(&bytes)
                else {
                    continue;
                };
                targets.extend(target.paragraphs.into_iter().map(|p| p.text));
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
            out.push_str(&format!(
                "{}\n{} --> {}\n{}\n\n",
                i + 1,
                cue.start,
                cue.end,
                target
            ));
        }
        std::fs::write(path, out).map_err(|e| invalid(format!("写 SRT 失败：{e}")))?;
        Ok(cues.len())
    }
}
