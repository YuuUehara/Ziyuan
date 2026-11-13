//! 人工译稿终端入口。业务判断全部调用应用层，与命令行同一组方法。
use ziyuan_app::translation::{
    DraftParagraph, DraftSave, TargetMark, Translations, VersionState, human_submit_request_id,
};
use ziyuan_storage::{LockScope, Workspace};

use crate::App;

#[derive(Clone)]
pub(crate) struct HumanParagraph {
    pub id: String,
    pub source: String,
    pub text: String,
    pub marks: Vec<TargetMark>,
}

#[derive(Default)]
pub(crate) struct HumanPanel {
    pub projects: Vec<String>,
    pub project: usize,
    pub blocks: Vec<String>,
    pub block: usize,
    pub paragraphs: Vec<HumanParagraph>,
    pub selected: usize,
    pub revision: Option<String>,
    pub basis: Option<ziyuan_core::DraftBasis>,
    pub saves: u64,
    loaded: bool,
}

impl App {
    pub(crate) fn ensure_human(&mut self) {
        if !self.human.loaded {
            self.reload_human();
        }
    }

    pub(crate) fn reload_human(&mut self) {
        let root = self.config.root.clone();
        let translations = Translations::new(&root);
        let projects = match translations.texts().list() {
            Ok(projects) => projects.into_iter().map(|project| project.id).collect(),
            Err(error) => {
                self.error(error.to_string());
                self.human.projects.clear();
                self.human.loaded = true;
                return;
            }
        };
        self.human.projects = projects;
        if self.human.project >= self.human.projects.len() {
            self.human.project = 0;
        }
        self.human.loaded = true;
        self.load_human_block();
    }

    pub(crate) fn load_human_block(&mut self) {
        let Some(project) = self.human.projects.get(self.human.project).cloned() else {
            self.human.blocks.clear();
            self.human.paragraphs.clear();
            self.human.revision = None;
            self.human.basis = None;
            return;
        };
        let translations = Translations::new(&self.config.root);
        let stored = match translations.project(&project) {
            Ok(project) => project,
            Err(error) => {
                self.error(error.to_string());
                return;
            }
        };
        self.human.blocks = stored
            .chapters
            .iter()
            .flat_map(|chapter| chapter.blocks.iter().map(|block| block.block_id.clone()))
            .collect();
        if self.human.block >= self.human.blocks.len() {
            self.human.block = 0;
        }
        let Some(block) = self.human.blocks.get(self.human.block).cloned() else {
            self.human.paragraphs.clear();
            return;
        };
        let source = stored
            .chapters
            .iter()
            .flat_map(|chapter| chapter.blocks.iter())
            .find(|item| item.block_id == block);
        let Some(source) = source else {
            self.error("找不到这一块");
            return;
        };
        let view = match translations.peek_human_draft(&project, &block) {
            Ok(view) => view,
            Err(error) => {
                self.error(error.to_string());
                return;
            }
        };
        self.human.basis = Some(view.basis);
        self.human.revision = view.draft.as_ref().map(|draft| draft.revision.clone());
        self.human.paragraphs = source
            .paragraphs
            .iter()
            .map(|paragraph| {
                let saved = view
                    .draft
                    .as_ref()
                    .and_then(|draft| draft.paragraphs.iter().find(|item| item.id == paragraph.id));
                HumanParagraph {
                    id: paragraph.id.clone(),
                    source: paragraph.text.clone(),
                    text: saved.map(|item| item.text.clone()).unwrap_or_default(),
                    marks: saved.map(|item| item.marks.clone()).unwrap_or_default(),
                }
            })
            .collect();
        if self.human.selected >= self.human.paragraphs.len() {
            self.human.selected = 0;
        }
    }

    pub(crate) fn shift_human_project(&mut self, delta: isize) {
        if self.human.projects.is_empty() {
            return;
        }
        self.human.project = shift(self.human.project, self.human.projects.len(), delta);
        self.human.block = 0;
        self.load_human_block();
    }

    pub(crate) fn shift_human_block(&mut self, delta: isize) {
        if self.human.blocks.is_empty() {
            return;
        }
        self.human.block = shift(self.human.block, self.human.blocks.len(), delta);
        self.load_human_block();
    }

    pub(crate) fn set_human_paragraph(&mut self, text: String) {
        let Some(paragraph) = self.human.paragraphs.get_mut(self.human.selected) else {
            return;
        };
        if paragraph.text != text {
            paragraph.text = text;
            paragraph.marks.clear();
        }
    }

    pub(crate) fn save_human(&mut self) {
        let Some(project) = self.current_human_project() else {
            self.error("尚无文本项目");
            return;
        };
        let Some(block) = self.current_human_block() else {
            self.error("这一项目还没有块");
            return;
        };
        let Some(basis) = self.human.basis.clone() else {
            self.error("还没有读到这一块，请按 r 重新读取");
            return;
        };
        self.human.saves += 1;
        let save = DraftSave {
            request_id: format!("tui-save-{}", self.human.saves),
            expected_revision: self.human.revision.clone(),
            basis,
            paragraphs: self
                .human
                .paragraphs
                .iter()
                .map(|paragraph| DraftParagraph {
                    id: paragraph.id.clone(),
                    text: paragraph.text.clone(),
                    marks: paragraph.marks.clone(),
                })
                .collect(),
        };
        match Translations::new(&self.config.root).save_human_draft(&project, &block, &save) {
            Ok(receipt) => {
                self.info(format!(
                    "已保存人工草稿：{project} / {block}（修订号 {}）。下一步按 u 送审",
                    receipt.revision
                ));
                self.load_human_block();
            }
            Err(error) => self.error(error.to_string()),
        }
    }

    pub(crate) fn submit_human(&mut self) {
        let Some((project, block)) = self.human_target() else {
            return;
        };
        let root = self.config.root.clone();
        let outcome = (|| {
            let translations = Translations::new(&root);
            let view = translations.peek_human_draft(&project, &block)?;
            let draft = view.draft.ok_or_else(|| {
                ziyuan_app::translation::TranslationError::Invalid(
                    "尚无人工草稿，请先按 s 保存".into(),
                )
            })?;
            let request_id = human_submit_request_id(&draft.revision);
            let binding = translations.prepare_human_submission(
                &project,
                &block,
                &request_id,
                &request_id,
                &draft.revision,
            )?;
            let workspace =
                Workspace::open(&root).map_err(|error| invalid_text(error.to_string()))?;
            let mut guard = workspace
                .lock(LockScope::Workspace)
                .map_err(|error| invalid_text(error.to_string()))?;
            guard
                .upgrade_write_protocol()
                .map_err(|error| invalid_text(error.to_string()))?;
            translations
                .texts()
                .upgrade_project_protocol(&project, &guard)
                .map_err(|error| invalid_text(error.to_string()))?;
            let published = translations.publish_human_binding(&guard, &binding)?;
            Ok::<_, ziyuan_app::translation::TranslationError>(published)
        })();
        match outcome {
            Ok(true) => self.info(format!("已送审：{project} / {block}。下一步按 a 准奏")),
            Ok(false) => self.info(format!(
                "这次送审此前已入簿，未重复追加：{project} / {block}"
            )),
            Err(error) => self.error(error.to_string()),
        }
        self.reload();
        self.load_human_block();
    }

    pub(crate) fn approve_human(&mut self) {
        self.finish_human(true);
    }

    pub(crate) fn finalize_human(&mut self) {
        self.finish_human(false);
    }

    fn finish_human(&mut self, approve: bool) {
        let Some((project, block)) = self.human_target() else {
            return;
        };
        let root = self.config.root.clone();
        let outcome = (|| {
            let translations = Translations::new(&root);
            let workspace =
                Workspace::open(&root).map_err(|error| invalid_text(error.to_string()))?;
            let mut guard = workspace
                .lock(LockScope::Workspace)
                .map_err(|error| invalid_text(error.to_string()))?;
            guard
                .upgrade_write_protocol()
                .map_err(|error| invalid_text(error.to_string()))?;
            translations
                .texts()
                .upgrade_project_protocol(&project, &guard)
                .map_err(|error| invalid_text(error.to_string()))?;
            let changed = if approve {
                translations.approve_human_submission(&mut guard, &project, &block)?
            } else {
                translations.finalize_human_translation(&guard, &project, &block)?
            };
            let _ = translations.auto_snapshot(
                &project,
                &block,
                if approve {
                    VersionState::Approved
                } else {
                    VersionState::Finalized
                },
            );
            Ok::<_, ziyuan_app::translation::TranslationError>(changed)
        })();
        match (approve, outcome) {
            (true, Ok(true)) => self.info(format!(
                "已准奏并落盘：{project} / {block}。下一步按 f 定稿"
            )),
            (true, Ok(false)) => self.info(format!(
                "这次准奏此前已入簿并落盘，未重复追加：{project} / {block}"
            )),
            (false, Ok(true)) => self.info(format!("已定稿：{project} / {block}")),
            (false, Ok(false)) => self.info(format!(
                "这次定稿此前已入簿，未重复追加：{project} / {block}"
            )),
            (_, Err(error)) => self.error(error.to_string()),
        }
        self.reload();
        self.load_human_block();
    }

    pub(crate) fn apply_human(&mut self) {
        let Some((project, block)) = self.human_target() else {
            return;
        };
        match Translations::new(&self.config.root).apply_human_target(&project, &block) {
            Ok(approval) => self.info(format!(
                "准奏时已落盘，本命令只作核对、未写任何文件：{project} / {block}（准奏 {approval}）。下一步按 f 定稿"
            )),
            Err(error) => self.error(error.to_string()),
        }
    }

    pub(crate) fn current_human_project(&self) -> Option<String> {
        self.human.projects.get(self.human.project).cloned()
    }

    pub(crate) fn current_human_block(&self) -> Option<String> {
        self.human.blocks.get(self.human.block).cloned()
    }

    fn human_target(&mut self) -> Option<(String, String)> {
        let project = self.current_human_project();
        let block = self.current_human_block();
        match (project, block) {
            (Some(project), Some(block)) => Some((project, block)),
            _ => {
                self.error("请先选择项目和块");
                None
            }
        }
    }
}

fn shift(index: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let next = index as isize + delta;
    if next < 0 {
        len - 1
    } else if next as usize >= len {
        0
    } else {
        next as usize
    }
}

fn invalid_text(message: String) -> ziyuan_app::translation::TranslationError {
    ziyuan_app::translation::TranslationError::Invalid(message)
}
