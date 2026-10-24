//! 事件簿 v1 文本编码：每行一个事件，字段以 Tab 分隔，自由文本以 hex 编码。
use ziyuan_core::{
    Agent, AgentId, Capability, CensorVerdict, CourtEvent, Department, Edict, Evidence, FileChange,
    GovernanceMode, Office, Seal, TaskId, TokenUsage, Verdict,
};

fn to_hex(value: &str) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(value.len() * 2);
    for b in value.bytes() {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 15) as usize] as char);
    }
    out
}
fn from_hex(s: &str) -> Result<String, String> {
    if !s.len().is_multiple_of(2) {
        return Err("hex 长度不合法".into());
    }
    let mut out = Vec::new();
    for pair in s.as_bytes().chunks(2) {
        let text = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
        out.push(u8::from_str_radix(text, 16).map_err(|e| e.to_string())?);
    }
    String::from_utf8(out).map_err(|e| e.to_string())
}
pub(crate) fn encode_event(event: &CourtEvent) -> String {
    use CourtEvent::*;
    match event {
        Founded(mode) => format!("v1\tFOUNDED\t{}", mode.code()),
        AgentRegistered(a) => format!(
            "v1\tREGISTERED\t{}\t{}\t{}",
            a.id.0,
            to_hex(&a.name),
            a.speciality.code()
        ),
        OfficialAppointed { agent, office } => {
            format!("v1\tAPPOINTED\t{}\t{}", agent.0, office.code())
        }
        OfficialDismissed { agent } => format!("v1\tDISMISSED\t{}", agent.0),
        OfficialPurged { agent } => format!("v1\tPURGED\t{}", agent.0),
        EdictApproved { task } => format!("v1\tAPPROVED\t{}", task.0),
        EdictDrafted {
            task,
            worker,
            note,
            model,
            usage,
        } => {
            let line = format!(
                "v1\tDRAFTED\t{}\t{}\t{}\t{}\t{}",
                task.0,
                worker.0,
                to_hex(note),
                usage.input,
                usage.output
            );
            match model {
                Some(model) => format!("{line}\t{}", to_hex(model)),
                None => line,
            }
        }
        InstitutionReformed(mode) => format!("v1\tREFORMED\t{}", mode.code()),
        EdictIssued(e) => format!(
            "v1\tEDICT\t{}\t{}\t{}\t{}",
            e.id.0,
            e.department.code(),
            to_hex(&e.text),
            encode_seal(&e.seal)
        ),
        OfficeModelSet { office, model } => match model {
            Some(model) => format!("v1\tMODEL\t{}\t{}", office.code(), to_hex(model)),
            None => format!("v1\tMODEL\t{}", office.code()),
        },
        OfficeCeilingSet { office, ceiling } => match ceiling {
            Some(seal) => format!("v1\tCEILING\t{}\t{}", office.code(), encode_seal(seal)),
            None => format!("v1\tCEILING\t{}", office.code()),
        },
        TaskCompleted {
            task,
            worker,
            summary,
        } => format!(
            "v1\tCOMPLETED\t{}\t{}\t{}",
            task.0,
            worker.0,
            to_hex(summary)
        ),
        TaskAssigned {
            task,
            worker,
            model: None,
        } => format!("v1\tASSIGNED\t{}\t{}", task.0, worker.0),
        TaskAssigned {
            task,
            worker,
            model: Some(model),
        } => format!("v1\tASSIGNED\t{}\t{}\t{}", task.0, worker.0, to_hex(model)),
        ToolInvoked {
            task,
            tool,
            args_json,
            ok,
        } => format!(
            "v1\tTOOL\t{}\t{}\t{}\t{}",
            task.0,
            to_hex(tool),
            to_hex(args_json),
            u8::from(*ok)
        ),
        TaskFailed { task, reason } => format!("v1\tFAILED\t{}\t{}", task.0, to_hex(reason)),
        TaskCancelled { task } => format!("v1\tCANCELLED\t{}", task.0),
        // 每处改动为 `路径:原文:新文`（均 hex，新建文件的原文为 `-`），以逗号分隔。
        ChangesProposed { task, changes } => {
            let entries: Vec<_> = changes
                .iter()
                .map(|c| {
                    let before = c.before.as_deref().map_or("-".into(), to_hex);
                    format!("{}:{before}:{}", to_hex(&c.path), to_hex(&c.after))
                })
                .collect();
            format!("v1\tCHANGES\t{}\t{}", task.0, entries.join(","))
        }
        ChangesApplied { task } => format!("v1\tAPPLIED\t{}", task.0),
        CensorReported {
            task,
            censor,
            verdict,
            note,
            evidence,
            model,
            usage,
        } => {
            let tests = match evidence.tests {
                None => "-",
                Some(true) => "1",
                Some(false) => "0",
            };
            let line = format!(
                "v1\tCENSURED\t{}\t{}\t{}\t{}\t{}\t{}\t{tests}\t{}\t{}",
                task.0,
                censor.0,
                verdict.code(),
                to_hex(note),
                evidence.cited,
                evidence.missing,
                usage.input,
                usage.output
            );
            match model {
                Some(model) => format!("{line}\t{}", to_hex(model)),
                None => line,
            }
        }
        ChangesDiscarded { task } => format!("v1\tDISCARDED\t{}", task.0),
        ModelUsage { task, usage } => {
            format!("v1\tUSAGE\t{}\t{}\t{}", task.0, usage.input, usage.output)
        }
        TaskReviewed {
            task,
            verdict,
            note,
        } => format!(
            "v1\tREVIEWED\t{}\t{}\t{}",
            task.0,
            verdict.code(),
            to_hex(note)
        ),
    }
}
/// 玉玺占两个字段：工具代码列表、hex 编码的路径列表。
fn encode_seal(seal: &Seal) -> String {
    let tools: Vec<_> = seal.capabilities.iter().map(|c| c.code()).collect();
    let scope: Vec<_> = seal.scope.iter().map(|p| to_hex(p)).collect();
    format!("{}\t{}", tools.join(","), scope.join(","))
}

fn decode_seal(tools: &str, scope: &str) -> Result<Seal, String> {
    Ok(Seal {
        capabilities: split_list(tools)
            .map(|c| c.parse::<Capability>().map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?,
        scope: split_list(scope).map(from_hex).collect::<Result<_, _>>()?,
    })
}

/// 逗号分隔的列表；空字段为空列表。
fn split_list(field: &str) -> impl Iterator<Item = &str> {
    field.split(',').filter(|item| !item.is_empty())
}

pub(crate) fn decode_event(line: &str) -> Result<CourtEvent, String> {
    use CourtEvent::*;
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.first() != Some(&"v1") {
        return Err("不支持的事件格式版本".into());
    }
    let field = |i: usize| parts.get(i).copied().ok_or_else(|| format!("缺少字段 {i}"));
    let id =
        |i: usize| -> Result<u64, String> { field(i)?.parse::<u64>().map_err(|e| e.to_string()) };
    let event = match field(1)? {
        "FOUNDED" => Founded(
            field(2)?
                .parse::<GovernanceMode>()
                .map_err(|e| e.to_string())?,
        ),
        "REGISTERED" => AgentRegistered(Agent {
            id: AgentId(id(2)?),
            name: from_hex(field(3)?)?,
            speciality: field(4)?.parse::<Department>().map_err(|e| e.to_string())?,
        }),
        "APPOINTED" => OfficialAppointed {
            agent: AgentId(id(2)?),
            office: field(3)?.parse::<Office>().map_err(|e| e.to_string())?,
        },
        "DISMISSED" => OfficialDismissed {
            agent: AgentId(id(2)?),
        },
        "PURGED" => OfficialPurged {
            agent: AgentId(id(2)?),
        },
        "APPROVED" => EdictApproved {
            task: TaskId(id(2)?),
        },
        "DRAFTED" => EdictDrafted {
            task: TaskId(id(2)?),
            worker: AgentId(id(3)?),
            note: from_hex(field(4)?)?,
            usage: TokenUsage {
                input: id(5)?,
                output: id(6)?,
            },
            model: match parts.len() {
                7 => None,
                _ => Some(from_hex(field(7)?)?),
            },
        },
        "REFORMED" => InstitutionReformed(
            field(2)?
                .parse::<GovernanceMode>()
                .map_err(|e| e.to_string())?,
        ),
        "EDICT" => EdictIssued(Edict {
            id: TaskId(id(2)?),
            department: field(3)?.parse::<Department>().map_err(|e| e.to_string())?,
            text: from_hex(field(4)?)?,
            // 早期日志没有玉玺字段：当时的诏令实际拥有全部只读工具与整个项目。
            seal: if parts.len() == 5 {
                Seal::default()
            } else {
                decode_seal(field(5)?, field(6)?)?
            },
        }),
        "MODEL" => OfficeModelSet {
            office: field(2)?.parse::<Office>().map_err(|e| e.to_string())?,
            model: match parts.len() {
                3 => None,
                _ => Some(from_hex(field(3)?)?),
            },
        },
        "CEILING" => OfficeCeilingSet {
            office: field(2)?.parse::<Office>().map_err(|e| e.to_string())?,
            ceiling: match parts.len() {
                3 => None,
                _ => Some(decode_seal(field(3)?, field(4)?)?),
            },
        },
        "COMPLETED" => TaskCompleted {
            task: TaskId(id(2)?),
            worker: AgentId(id(3)?),
            summary: from_hex(field(4)?)?,
        },
        // v0.2 及更早的日志没有模型字段。
        "ASSIGNED" => TaskAssigned {
            task: TaskId(id(2)?),
            worker: AgentId(id(3)?),
            model: match parts.len() {
                4 => None,
                _ => Some(from_hex(field(4)?)?),
            },
        },
        "TOOL" => ToolInvoked {
            task: TaskId(id(2)?),
            tool: from_hex(field(3)?)?,
            args_json: from_hex(field(4)?)?,
            ok: match field(5)? {
                "1" => true,
                "0" => false,
                other => return Err(format!("成败标记无效: {other}")),
            },
        },
        "FAILED" => TaskFailed {
            task: TaskId(id(2)?),
            reason: from_hex(field(3)?)?,
        },
        "CANCELLED" => TaskCancelled {
            task: TaskId(id(2)?),
        },
        "CHANGES" => ChangesProposed {
            task: TaskId(id(2)?),
            changes: split_list(field(3)?)
                .map(|entry| -> Result<FileChange, String> {
                    let [path, before, after] = entry.split(':').collect::<Vec<_>>()[..] else {
                        return Err("改动字段不合法".into());
                    };
                    Ok(FileChange {
                        path: from_hex(path)?,
                        before: (before != "-").then(|| from_hex(before)).transpose()?,
                        after: from_hex(after)?,
                    })
                })
                .collect::<Result<_, _>>()?,
        },
        "APPLIED" => ChangesApplied {
            task: TaskId(id(2)?),
        },
        "CENSURED" => CensorReported {
            task: TaskId(id(2)?),
            censor: AgentId(id(3)?),
            verdict: field(4)?
                .parse::<CensorVerdict>()
                .map_err(|e| e.to_string())?,
            note: from_hex(field(5)?)?,
            evidence: Evidence {
                cited: u32::try_from(id(6)?).map_err(|e| e.to_string())?,
                missing: u32::try_from(id(7)?).map_err(|e| e.to_string())?,
                tests: match field(8)? {
                    "-" => None,
                    "1" => Some(true),
                    "0" => Some(false),
                    other => return Err(format!("测试标记无效: {other}")),
                },
            },
            usage: TokenUsage {
                input: id(9)?,
                output: id(10)?,
            },
            model: match parts.len() {
                11 => None,
                _ => Some(from_hex(field(11)?)?),
            },
        },
        "DISCARDED" => ChangesDiscarded {
            task: TaskId(id(2)?),
        },
        "USAGE" => ModelUsage {
            task: TaskId(id(2)?),
            usage: TokenUsage {
                input: id(3)?,
                output: id(4)?,
            },
        },
        "REVIEWED" => TaskReviewed {
            task: TaskId(id(2)?),
            verdict: field(3)?.parse::<Verdict>().map_err(|e| e.to_string())?,
            note: from_hex(field(4)?)?,
        },
        other => return Err(format!("未知事件类型: {other}")),
    };
    let expected_fields = match &event {
        Founded(_)
        | InstitutionReformed(_)
        | OfficialDismissed { .. }
        | OfficialPurged { .. }
        | EdictApproved { .. }
        | ChangesApplied { .. }
        | ChangesDiscarded { .. }
        | TaskCancelled { .. } => 3,
        ChangesProposed { .. } => 4,
        CensorReported { model: None, .. } => 11,
        CensorReported { .. } => 12,
        OfficeModelSet { model: None, .. } | OfficeCeilingSet { ceiling: None, .. } => 3,
        OfficialAppointed { .. }
        | TaskAssigned { model: None, .. }
        | OfficeModelSet { .. }
        | TaskFailed { .. } => 4,
        TaskAssigned { .. } => 5,
        EdictDrafted { model: None, .. } => 7,
        EdictDrafted { .. } => 8,
        OfficeCeilingSet { .. } => 5,
        EdictIssued(_) if parts.len() == 5 => 5,
        AgentRegistered(_) | TaskCompleted { .. } | TaskReviewed { .. } | ModelUsage { .. } => 5,
        EdictIssued(_) => 7,
        ToolInvoked { .. } => 6,
    };
    if parts.len() != expected_fields {
        return Err("字段数量不正确".into());
    }
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_round_trip_chinese_and_tabs() {
        let task = TaskId(42);
        let events = [
            CourtEvent::EdictIssued(Edict {
                id: task,
                department: Department::Works,
                text: "检查\tRust\n测试".into(),
                seal: Seal {
                    capabilities: [Capability::SearchCode].into(),
                    scope: vec!["crates/紫垣,a".into(), "README.md".into()],
                },
            }),
            CourtEvent::EdictIssued(Edict {
                id: TaskId(43),
                department: Department::Works,
                text: "无工具".into(),
                seal: Seal {
                    capabilities: Default::default(),
                    scope: vec![".".into()],
                },
            }),
            CourtEvent::TaskAssigned {
                task,
                worker: AgentId(7),
                model: None,
            },
            CourtEvent::TaskAssigned {
                task,
                worker: AgentId(7),
                model: Some("deepseek-flash".into()),
            },
            CourtEvent::OfficeModelSet {
                office: Office::Clerk(Department::Works),
                model: Some("deepseek-v4-pro".into()),
            },
            CourtEvent::OfficeModelSet {
                office: Office::Chancellor,
                model: None,
            },
            CourtEvent::OfficeCeilingSet {
                office: Office::Clerk(Department::Works),
                ceiling: Some(Seal {
                    capabilities: [Capability::ReadFile].into(),
                    scope: vec!["crates/紫垣".into()],
                }),
            },
            CourtEvent::OfficeCeilingSet {
                office: Office::Minister(Department::Works),
                ceiling: None,
            },
            CourtEvent::ToolInvoked {
                task,
                tool: "read_file".into(),
                args_json: r#"{"path":"src/\t主.rs"}"#.into(),
                ok: false,
            },
            CourtEvent::TaskFailed {
                task,
                reason: "超时\n（300s）".into(),
            },
            CourtEvent::TaskCancelled { task },
            CourtEvent::ChangesProposed {
                task,
                changes: vec![
                    FileChange {
                        path: "src/新:a.rs".into(),
                        before: None,
                        after: "fn a() {}\n".into(),
                    },
                    FileChange {
                        path: "README.md".into(),
                        before: Some(String::new()),
                        after: "\t,一\n".into(),
                    },
                ],
            },
            CourtEvent::ChangesApplied { task },
            CourtEvent::CensorReported {
                task,
                censor: AgentId(7),
                verdict: CensorVerdict::Impeached,
                note: "查无\t实据".into(),
                evidence: Evidence {
                    cited: 3,
                    missing: 1,
                    tests: Some(false),
                },
                model: Some("pro".into()),
                usage: TokenUsage {
                    input: 9,
                    output: 2,
                },
            },
            CourtEvent::CensorReported {
                task,
                censor: AgentId(7),
                verdict: CensorVerdict::Endorsed,
                note: String::new(),
                evidence: Evidence::default(),
                model: None,
                usage: TokenUsage::default(),
            },
            CourtEvent::ChangesDiscarded { task },
            CourtEvent::EdictApproved { task },
            CourtEvent::EdictDrafted {
                task,
                worker: AgentId(4),
                note: "拟派：#4\t熟悉此事".into(),
                model: Some("deepseek-v4-pro".into()),
                usage: TokenUsage {
                    input: 300,
                    output: 20,
                },
            },
            CourtEvent::EdictDrafted {
                task,
                worker: AgentId(7),
                note: "御笔改票".into(),
                model: None,
                usage: TokenUsage::default(),
            },
            CourtEvent::OfficialPurged { agent: AgentId(3) },
            CourtEvent::ModelUsage {
                task,
                usage: TokenUsage {
                    input: 12_345,
                    output: 678,
                },
            },
            CourtEvent::TaskReviewed {
                task,
                verdict: Verdict::Rejected,
                note: "结论有误\t需重查".into(),
            },
        ];
        for original in events {
            assert_eq!(decode_event(&encode_event(&original)).unwrap(), original);
        }
    }
    #[test]
    fn edict_without_seal_fields_gets_default_seal() {
        let CourtEvent::EdictIssued(edict) = decode_event("v1\tEDICT\t1\tworks\te69fa5").unwrap()
        else {
            panic!("应解码为诏令");
        };
        assert_eq!((edict.text.as_str(), edict.seal), ("查", Seal::default()));
        assert!(decode_event("v1\tEDICT\t1\tworks\te69fa5\tread_file").is_err());
        assert!(decode_event("v1\tEDICT\t1\tworks\te69fa5\trun_shell\t2e").is_err());
    }
    #[test]
    fn malformed_event_is_rejected() {
        assert!(decode_event("v1\tEDICT\t42\tworks\tbroken").is_err());
    }
}
