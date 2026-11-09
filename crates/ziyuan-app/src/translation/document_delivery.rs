//! Binary document frontends use the same importers and finished-approval gate.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
fn format_name(format: &str) -> Result<&str> {
    if matches!(format, "docx" | "epub" | "srt") {
        Ok(format)
    } else {
        Err(invalid("文件格式须为DOCX、EPUB或SRT"))
    }
}
impl Translations {
    fn document_stage(&self, project: &str) -> Result<std::path::PathBuf> {
        let directory = self.store_dir(project)?;
        backup::plain_parents(&directory)?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = directory.join(format!(
            ".document-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path)?;
        Ok(path)
    }
    pub fn import_document_bytes(
        &self,
        project: &str,
        format: &str,
        bytes: &[u8],
    ) -> Result<usize> {
        let format = format_name(format)?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(invalid("原件超过16 MiB"));
        }
        if !self.project(project)?.chapters.is_empty() {
            return Err(invalid("项目已有原文，不能重复导入"));
        }
        let stage = self.document_stage(project)?;
        let file = stage.join(format!("original.{format}"));
        let mut out = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&file)?;
        out.write_all(bytes)?;
        out.sync_all()?;
        drop(out);
        let result = match format {
            "docx" => self.import_docx(project, &file),
            "epub" => self.import_epub(project, &file),
            _ => self.import_srt(project, &file),
        };
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&stage);
        result
    }
    pub fn export_document_bytes(&self, project: &str, format: &str) -> Result<Vec<u8>> {
        let format = format_name(format)?;
        self.project(project)?;
        let mut session = crate::Session::open(ziyuan_storage::EventLog::new(self.root()))
            .map_err(|e| invalid(e.to_string()))?;
        let options = ExportOptions {
            format: ExportFormat::Html,
            ..Default::default()
        };
        let before = session
            .export_finished(self.root(), project, &options)
            .map_err(|e| invalid(e.to_string()))?;
        let stage = self.document_stage(project)?;
        let file = stage.join(format!("finished.{format}"));
        let written = match format {
            "docx" => self.export_docx(project, &file),
            "epub" => self.export_epub(project, &file),
            _ => self.export_srt(project, &file),
        };
        let result = written.and_then(|_| Ok(std::fs::read(&file)?));
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&stage);
        let bytes = result?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(invalid("成稿文件超过64 MiB"));
        }
        let after = session
            .export_finished(self.root(), project, &options)
            .map_err(|e| invalid(e.to_string()))?;
        if before.manifest != after.manifest {
            return Err(invalid("成稿期间材料有变，请重新预检"));
        }
        Ok(bytes)
    }
}
