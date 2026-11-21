//! 测试／检查执行工具：`run_tests`（在断网号舍临时副本中执行皇帝设定的测试命令）。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use ziyuan_core::Capability;

use crate::BoxError;
use crate::agent_loop::{Tool, ToolSpec};

use super::{
    MAX_FAILURE_SCAN, MAX_TEST_OUTPUT, SKIPPED_DIRS, Sandbox, TEST_ENV, TEST_TIMEOUT, failures,
    is_denied, object_args, optional_string,
};

pub(super) struct RunTests(pub(super) Arc<Sandbox>);

impl Tool for RunTests {
    fn spec(&self) -> ToolSpec {
        let default_cmd = self
            .resolve_command(None)
            .ok()
            .unwrap_or("（皇帝尚未设定）");
        let presets_note = if self.0.test_presets.is_empty() {
            String::new()
        } else {
            let list = self
                .0
                .test_presets
                .iter()
                .map(|(k, v)| format!("{k}=`{v}`"))
                .collect::<Vec<_>>()
                .join("、");
            format!("另可通过可选参数 `preset` 选用皇帝白名单中的预设命令：{list}。")
        };
        ToolSpec {
            name: Capability::RunTests.code().into(),
            description: format!(
                "在项目的临时副本（含已暂存的改动）中运行皇帝设定的检查／测试命令（默认 `{default_cmd}`），返回退出码与输出末尾（未通过时作为工具错误返回）。{presets_note}命令由皇帝预先定好，模型不能改写或拼接参数。若输出是 cargo test、pytest、node --test、go test、jest/vitest 或 rustc/tsc/mypy 编译诊断且未通过，另附结构化失败／诊断摘录；其他格式只给输出末尾。"
            ),
            input_schema_json: r#"{"type":"object","properties":{"preset":{"type":"string","description":"可选，从皇帝 ZIYUAN_TEST_COMMANDS 白名单中选用的预设名（如 check、clippy、test）；留空运行默认命令"}},"additionalProperties":false}"#
                .into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        self.execute_with_cancel(args_json, None)
    }

    fn execute_with_cancel(
        &self,
        args_json: &str,
        cancel: Option<&AtomicBool>,
    ) -> Result<String, BoxError> {
        let args = object_args(args_json, &["preset"])?;
        let preset = optional_string(&args, "preset")?;
        self.run_preset(preset.as_deref(), crate::haoshe::Mode::from_env()?, cancel)
    }
}

impl RunTests {
    pub(super) fn resolve_command(&self, preset: Option<&str>) -> Result<&str, BoxError> {
        if let Some(name) = preset {
            let name = name.trim();
            if name.is_empty() {
                return Err("preset 不能为空字符串（不指定预设请省略该字段）".into());
            }
            if let Some(cmd) = self.0.test_presets.get(name) {
                return Ok(cmd.as_str());
            }
            if self.0.test_presets.is_empty() {
                return Err(format!(
                    "皇帝尚未通过 ZIYUAN_TEST_COMMANDS 设定命名预设，不能使用 preset `{name}`"
                )
                .into());
            }
            let allowed = self
                .0
                .test_presets
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join("、");
            return Err(format!("未知测试预设 `{name}`；皇帝允许的预设仅限：{allowed}").into());
        }
        if let Some(cmd) = self.0.test_command.as_deref() {
            return Ok(cmd);
        }
        if let Some(cmd) = self.0.test_presets.get("default") {
            return Ok(cmd.as_str());
        }
        if let Some((_, cmd)) = self.0.test_presets.iter().next() {
            return Ok(cmd.as_str());
        }
        Err("皇帝尚未设定测试命令（ZIYUAN_TEST_COMMAND 或 ZIYUAN_TEST_COMMANDS）".into())
    }

    #[cfg(test)]
    pub(super) fn run(&self, mode: crate::haoshe::Mode) -> Result<String, BoxError> {
        self.run_preset(None, mode, None)
    }

    pub(super) fn run_preset(
        &self,
        preset: Option<&str>,
        mode: crate::haoshe::Mode,
        cancel: Option<&AtomicBool>,
    ) -> Result<String, BoxError> {
        let command = self.resolve_command(preset)?;
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            return Err(format!("测试 `{command}`：已取消（启动前已收到撤销信号）").into());
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "ziyuan-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let copy = base.join("project");
        let result = self.run_in_copy(command, mode, &base, &copy, cancel);
        let _ = fs::remove_dir_all(&base);
        result
    }

    fn run_in_copy(
        &self,
        command: &str,
        mode: crate::haoshe::Mode,
        base: &Path,
        copy: &Path,
        cancel: Option<&AtomicBool>,
    ) -> Result<String, BoxError> {
        copy_tree(&self.0.root, &self.0.root, copy)?;
        for change in self.0.staged.lock().expect("暂存区锁未中毒").values() {
            let target = copy.join(&change.path);
            if change.deleted {
                match fs::remove_file(&target) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(target, &change.after)?;
        }
        let target_dir: Vec<PathBuf> = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .into_iter()
            .collect();
        let env: Vec<(String, std::ffi::OsString)> = TEST_ENV
            .iter()
            .filter_map(|key| std::env::var_os(key).map(|v| ((*key).to_owned(), v)))
            .collect();
        let hidden = [self.0.root.join(".ziyuan")];
        let toolchains = crate::haoshe::toolchain_dirs_from_env();
        let run = crate::haoshe::run_shell(&crate::haoshe::ShellRunSpec {
            command,
            mode,
            network: crate::haoshe::Network::from_env()?,
            work_dir: copy,
            sandbox_base: base,
            extra_writable: &target_dir,
            readonly_toolchains: &toolchains,
            hidden: &hidden,
            env: &env,
            timeout: TEST_TIMEOUT,
            max_output: MAX_TEST_OUTPUT,
            max_failure_scan: MAX_FAILURE_SCAN,
            cancel,
        })?;
        let note = if run.tail_truncated {
            format!("（仅最后 {MAX_TEST_OUTPUT} 字节）")
        } else {
            String::new()
        };
        let mut report = format!(
            "测试 `{command}`：{}（{}）\n输出{note}：\n{}",
            run.verdict, run.haoshe_note, run.tail
        );
        if !run.passed
            && let Some(scan) = run.failure_scan.as_deref()
            && let Some(failures) = failures::failure_note(scan)
        {
            report.push('\n');
            report.push_str(&failures);
        }
        // 未通过记为工具失败：审计中 `run_tests` 的成败即测试的成败，供都察院核验。
        if run.passed {
            Ok(report)
        } else {
            Err(report.into())
        }
    }
}

/// 将项目根目录复制到任务私有目录，跳过禁区（`.ziyuan`、`.git`、`.jj`、`.env*`）、构建产物与符号链接／重解析点。
#[cfg(windows)]
pub(crate) fn copy_project_tree(root: &Path, dest: &Path) -> io::Result<()> {
    copy_tree(root, root, dest)
}

/// 复制 `from` 到 `to`：跳过禁区、构建产物与符号链接／重解析点。
fn copy_tree(root: &Path, from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if is_denied(root, &path) || SKIPPED_DIRS.iter().any(|d| name == *d) {
            continue;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
            if fs::symlink_metadata(&path)
                .map(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
                .unwrap_or(true)
            {
                continue;
            }
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_tree(root, &path, &to.join(&name))?;
        } else if file_type.is_file() {
            fs::copy(&path, to.join(&name))?;
        }
    }
    Ok(())
}
