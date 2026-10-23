//! 事件簿 v1 文本编码：每行一个事件，字段以 Tab 分隔，自由文本以 hex 编码。
use ziyuan_core::{
    Agent, AgentId, Capability, CourtEvent, Department, Edict, GovernanceMode, Office, Seal,
    TaskId, TokenUsage, Verdict,
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
        InstitutionReformed(mode) => format!("v1\tREFORMED\t{}", mode.code()),
        EdictIssued(e) => format!(
            "v1\tEDICT\t{}\t{}\t{}\t{}\t{}",
            e.id.0,
            e.department.code(),
            to_hex(&e.text),
            e.seal
                .capabilities
                .iter()
                .map(|c| c.code())
                .collect::<Vec<_>>()
                .join(","),
            e.seal
                .scope
                .iter()
                .map(|p| to_hex(p))
                .collect::<Vec<_>>()
                .join(",")
        ),
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
        TaskAssigned { task, worker } => format!("v1\tASSIGNED\t{}\t{}", task.0, worker.0),
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
                Seal {
                    capabilities: split_list(field(5)?)
                        .map(|c| c.parse::<Capability>().map_err(|e| e.to_string()))
                        .collect::<Result<_, _>>()?,
                    scope: split_list(field(6)?)
                        .map(from_hex)
                        .collect::<Result<_, _>>()?,
                }
            },
        }),
        "COMPLETED" => TaskCompleted {
            task: TaskId(id(2)?),
            worker: AgentId(id(3)?),
            summary: from_hex(field(4)?)?,
        },
        "ASSIGNED" => TaskAssigned {
            task: TaskId(id(2)?),
            worker: AgentId(id(3)?),
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
        | TaskCancelled { .. } => 3,
        OfficialAppointed { .. } | TaskAssigned { .. } | TaskFailed { .. } => 4,
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
            CourtEvent::EdictApproved { task },
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
        assert!(decode_event("v1\tEDICT\t1\tworks\te69fa5\twrite_file\t2e").is_err());
    }
    #[test]
    fn malformed_event_is_rejected() {
        assert!(decode_event("v1\tEDICT\t42\tworks\tbroken").is_err());
    }
}
