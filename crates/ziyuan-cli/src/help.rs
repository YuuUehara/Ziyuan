//! 帮助文本、子命令清单与 shell 补全脚本。

use std::env;
use ziyuan_core::{Capability, Department, GovernanceMode, Office};

pub(crate) const HELP: &str = concat!(
    "紫垣 ZiYuan ",
    env!("CARGO_PKG_VERSION"),
    r#" — 朝廷式多 Agent 框架（演示版）

用法：
  ziyuan init                          初始化朝廷（在当前目录建立 .ziyuan/）
  ziyuan court                         查看制度与朝廷状态
  ziyuan officials                     查看官员、官职、品级、所用模型及任务完成数
  ziyuan recruit [姓名] <部门>          招募新官（不给姓名则随机取名；须另行 appoint 任职）
  ziyuan edict [--template <模板>] [--dept works] [--scope <路径>]... [--tools <工具>] [--mcp <授权>] [--queue] <内容>
                                       下诏并立即执行；--queue 只入队（默认模拟后端；见下方环境变量）
  ziyuan run [--jobs 4] [--timeout 300]
                                       并发执行全部未结束的诏令；Ctrl-C 撤销执行中的诏令
  ziyuan run --watch [--poll 5] [--no-censor]
                                       常朝：常驻执行，驿传与入队的诏令随到随办，办完自动请都察院复核；Ctrl-C 退朝
  ziyuan tasks                         查看诏令及其状态
  ziyuan serve [--port 8080] [--bind 127.0.0.1] [--new-token]
                                       启动紫禁城 Web 界面（默认仅本机；令牌存于 .ziyuan/web-token）
  ziyuan tui                           启动紫禁城终端界面
  ziyuan migrate                       检查事件簿：本版能否读，至少要哪个版本才能打开
  ziyuan login [--models]              用 ChatGPT 订阅额度调用模型：浏览器登录；--models 列出可选模型
  ziyuan logout                        退出 ChatGPT 订阅登录（撤销令牌）
  ziyuan cancel <诏令ID>                撤销未结束的诏令
  ziyuan reform <chancellor|direct|cabinet>
  ziyuan retreat                       修道怠政（昏君模式）：批红、评定由司礼监代批，记入事件簿
  ziyuan return                        回朝亲政
  ziyuan dismiss <官员ID>               罢免现任官职
  ziyuan purge <官员ID>                 革职：削去仕籍，永不叙用
  ziyuan abolish                       一键朱元璋：罢中书省，株连百官（全部在任官员革职削籍）
  ziyuan model <官职> <模型|default>     为官职指定承办模型（同一服务地址与 Key）
  ziyuan ceiling <官职> [--scope <路径>]... [--tools <工具>] [--mcp <授权>] | default
                                       设定官职的玉玺上限：该职承办时，诏令玉玺与上限取交集
  ziyuan split <诏令ID> [官员ID:子诏令]...
                                       多官协作：尚书（模型）把待办诏令拆给本部官员分办，或皇帝御定；
                                       子诏令并发执行，全部结束后尚书汇总回奏
  ziyuan debate <诏令ID> [--rounds 2] [官员ID]...
                                       廷议：数位在任官员（默认宰辅、本部长官、御史）按各自性格文风
                                       就待办诏令轮流辩论；纪要供票拟与承办参考，不改变派工与玉玺
  ziyuan draft <诏令ID|all>             内阁票拟：首辅（模型）拟定承办人并附意见
  ziyuan redraft <诏令ID> <官员ID> [理由] 御笔改票：改定拟派的承办人
  ziyuan approve <诏令ID|all>           内阁制下批红（批红后方可执行；未票拟即批红为中旨，按旧例派工）
  ziyuan appoint <官员ID> <官职>         任命（如 first-secretary / minister:works）
  ziyuan diff <诏令ID>                  查看工部呈报的改动（统一 diff）
  ziyuan apply <诏令ID> [--skip 1,3] [--commit]
                                       准奏：把呈报的改动写入项目（文件在此期间被改过则拒绝）；
                                       --skip 逐段驳回 diff 中所列段号，其余落盘；
                                       --commit 落盘后只把这些文件记为一个 jj（或 git）提交
  ziyuan discard <诏令ID>               驳回呈报的改动，不写入
  ziyuan review <诏令ID> <accept|reject> [备注]
                                       评定已完成的诏令（吏部考绩只采信评定）
  ziyuan censor <诏令ID|all>            都察院复核回奏：核验引用与测试，御史背书或弹劾
  ziyuan budget [court|<官职>|<诏令ID> <tokens|none>]
                                       户部度支：设朝廷总额、官职每件或某件诏令的 token 上限，用尽即停支；
                                       不带参数则出具度支奏销（按用途、模型、官员汇总花费）
  ziyuan post [--dept works] [--station 驿站] [--urgent] <内容|->
                                         兵部驿传：外部事件（CI 失败、新 issue）入队为诏令，`-` 读标准输入；
                                         默认玉玺（只读），仍须批红、受度支约束；只入队，不执行
  ziyuan urgent <诏令ID>                 八百里加急：插队先行，时限加倍
  ziyuan template                        翰林院：列出诏令模板（内置「审查unsafe」「补测试」「写文档」）
  ziyuan template save <名> [--dept works] [--scope <路径>]... [--tools <工具,…>|none] --criteria <甲；乙> <内容>
                                         存模板（同名覆盖，可覆盖内置的）；验收标准以「；」分条
  ziyuan template rm <名>                撤去自定的模板（内置的恢复原样）
  ziyuan edict --template <名> [补充内容]  按模板下诏；--dept/--scope/--tools 可覆盖模板的默认值
  ziyuan exam <官员ID>                   礼部科举：官员应试 .ziyuan/exam/ 下的考题，按要点判分，记入履历
  ziyuan exam --palace <题目>            殿试：天子亲策，在任官员各答一卷
  ziyuan crown <官员ID>                  钦点状元（须为最近一次殿试的答卷者）
  ziyuan nominate <官职> [--lots]        廷推：官缺时吏部会同九卿各推一人（只是建议）；--lots 枚卜，从被推者中抽签任命
  ziyuan cane <官员ID>                   廷杖：屡遭弹劾（两次以上）者降一级留用，收回 write_file、run_tests，三件获采纳即复
  ziyuan pardon <官员ID>                 宽宥受杖者，复其品级与玉玺
  ziyuan flags [诏令ID]                  刑部与锦衣卫的密报：外来文书中的劫持指令、回奏中的越权与索取机密
  ziyuan mcp                            外藩名册：.ziyuan/mcp.json 登记的 MCP 服务及其工具
  ziyuan changelog [--version 0.5.0] [--since v0.4.0]
                                       礼部修 CHANGELOG：据提交与实录起草，只可写 CHANGELOG.md，须准奏落盘（只入队）
  ziyuan github pull [--label ziyuan] [--dept works] [--tools <工具,…>] [--scope <路径>]...
                                       兵部驿站·GitHub：带标签的 issue/PR 及成员的 `/ziyuan` 评论入队（去重）；
                                       默认只读，--tools 可授工部写权（如 read_file,write_file），之后方能 `github pr`
  ziyuan github reply <诏令ID|all>      已评定、无未平反密报的回奏写回 issue/PR 评论
  ziyuan github pr <诏令ID>             准奏的改动在远端开为草稿 PR（先核对远端原文；不碰本地版本库）
  ziyuan recheck <诏令ID|all>            刑部复勘：刑部官员（模型）逐处判断检出是真有其事还是误报，只是意见
  ziyuan acquit <诏令ID> [序号,…|all]    平反：不给序号则平反刑部判为误报的各处；平反后不再妨碍代批
  ziyuan kaocheng                      考成簿：在任官员的功过与等第（仿张居正考成法）
  ziyuan examine <官员ID>               吏部考绩：经评定与复核的履历
  ziyuan recommend <官职>               吏部荐举：按履历排列可任该职者（不自动任命）
  ziyuan history [--task ID] [--official ID] [--kind 类型] [--since 时间] [--until 时间]
                                       查看起居注（事件记录）；时间如 30m、2h、3d、2026-10-25、2026-10-25 14:30，
                                       按 ZIYUAN_TZ（如 +08:00，默认 UTC）
  ziyuan records [检索词]                翰林院实录：已结诏令的诏令、回奏与评定（只增不改）；给检索词则查前例
  ziyuan <court|officials|tasks|history|records> --json
                                       以 JSON 输出（court 与 Web 的 /api/state 同构），便于脚本使用
  ziyuan completions <bash|zsh|fish>   输出 shell 补全脚本（如 `ziyuan completions bash > ~/.local/share/bash-completion/completions/ziyuan`）
  ziyuan help

官制（品级随制度而变）：
  丞相制（仿元·中书省）  chancellor 中书右丞相、vice-chancellor 中书左丞相
  废相亲政（仿明初）    不设宰辅，皇帝直领六部
  内阁制（仿明中后期）  first-secretary 内阁首辅、second-secretary 内阁次辅
  六部（各制度通用）    minister:<部门> 尚书、vice-minister:<部门> 侍郎、clerk:<部门> 郎中
  都察院（各制度通用）  censor 左都御史（元称御史台御史大夫），不入派工路线
  正职空缺时由副职署理。
部门：personnel / revenue / rites / war / justice / works

玉玺（承办官员的权限，随诏令记入事件簿）：
  --scope <路径>   可查阅的项目路径（目录或文件，可重复）；默认整个项目
  --tools <工具>   read_file、search_code、write_file、run_tests 的逗号列表，或 none；
                   默认 read_file,search_code（只读）
  --mcp <授权>     外藩 MCP 工具（.ziyuan/mcp.json 登记的服务）：服务、服务/工具 或 * 的逗号列表；
                   默认不授。外部工具不受 --scope 约束，只授信得过的服务
  write_file、run_tests 只能授予工部：改动只暂存，诏令完成后呈报，须 `ziyuan apply` 准奏才落盘；
  run_tests 在项目的临时副本（含暂存改动）中运行 ZIYUAN_TEST_COMMAND，不传 API Key 等环境变量。
  无论玉玺如何，.ziyuan、.git、.jj 与 .env* 均不可读写。

真实模型（OpenAI 兼容 Chat Completions API）：
  ZIYUAN_LLM_MODEL     模型名；设置后 edict 由模型以只读工具查阅当前目录
  ZIYUAN_LLM_BASE_URL  默认 https://api.openai.com/v1（如 https://api.deepseek.com/v1、http://localhost:11434/v1）
  ZIYUAN_LLM_API_KEY   API Key（本地 Ollama 可不设）
  ZIYUAN_TEST_COMMAND  run_tests 运行的测试命令（如 cargo test；由皇帝设定，模型不能更改）
  ZIYUAN_LLM_PRICE     估算花费用的单价「输入,输出[,币种]」（每百万 token）；
                       deepseek-flash / deepseek-v4-pro 内置官方高峰价（估算上限）
  ZIYUAN_LLM_RETRIES   网络错误、429、5xx 时的重试次数（默认 3，指数退避，遵守 Retry-After）
  ZIYUAN_MAX_STEPS     单件诏令最多的模型调用步数（默认 12）；用尽后官员不带工具据已查阅的内容回奏

示例：
  ziyuan init
  ziyuan edict --dept works "检查 Rust 项目的 unsafe 代码"
  ziyuan reform direct
  ziyuan edict --dept works "重新检查"
  ziyuan reform cabinet
  ziyuan dismiss 6
  ziyuan appoint 6 first-secretary
  ziyuan edict --dept works "请内阁协调检查"
  ziyuan edict --queue --scope crates/ziyuan-storage --tools search_code "检查 storage crate"
  ziyuan edict --queue "检查 runtime crate"
  ziyuan run --jobs 2 --timeout 120
"#
);

#[derive(Clone, Copy)]
pub(crate) enum Shell {
    Bash,
    Zsh,
    Fish,
}

/// 全部子命令（补全用；测试保证每个都能被 `parse` 识别）。
pub(crate) const SUBCOMMANDS: &[&str] = &[
    "help",
    "init",
    "court",
    "officials",
    "history",
    "records",
    "recruit",
    "edict",
    "template",
    "run",
    "tasks",
    "tui",
    "migrate",
    "login",
    "logout",
    "serve",
    "cancel",
    "retreat",
    "return",
    "reform",
    "dismiss",
    "purge",
    "abolish",
    "model",
    "ceiling",
    "draft",
    "redraft",
    "approve",
    "appoint",
    "examine",
    "diff",
    "kaocheng",
    "post",
    "urgent",
    "exam",
    "crown",
    "nominate",
    "cane",
    "pardon",
    "flags",
    "recheck",
    "acquit",
    "github",
    "changelog",
    "mcp",
    "budget",
    "split",
    "debate",
    "censor",
    "apply",
    "discard",
    "review",
    "recommend",
    "completions",
];

/// 支持 `--json` 的子命令。
pub(crate) const JSON_COMMANDS: &[&str] = &["court", "officials", "tasks", "history", "records"];

/// shell 补全脚本：首词补子命令；官职、部门、制度、工具等参数按所在子命令补全。
pub(crate) fn completions(shell: Shell) -> String {
    let words = |items: Vec<String>| items.join(" ");
    let offices = words(Office::all().into_iter().map(|o| o.code()).collect());
    let departments = words(
        Department::ALL
            .iter()
            .map(|d| d.code().to_owned())
            .collect(),
    );
    let modes = words(
        GovernanceMode::ALL
            .iter()
            .map(|m| m.code().to_owned())
            .collect(),
    );
    let tools = words(
        Capability::ALL
            .iter()
            .map(|c| c.code().to_owned())
            .collect(),
    );
    let commands = SUBCOMMANDS.join(" ");
    let json = JSON_COMMANDS.join(" ");
    let bash = format!(
        r#"_ziyuan() {{
  # 官职代码含冒号，而 bash 默认在冒号处分词：从 COMP_LINE 取完整的当前词。
  local line="${{COMP_LINE:0:$COMP_POINT}}" words=""
  local cur="${{line##*[[:space:]]}}" rest="${{line%[[:space:]]*}}"
  local prev="${{rest##*[[:space:]]}}"
  if [ "$COMP_CWORD" -eq 1 ]; then
    COMPREPLY=($(compgen -W "{commands}" -- "$cur")); return
  fi
  case "$prev" in
    --dept) words="{departments}" ;;
    --tools) words="{tools} none" ;;
    *) case "${{COMP_WORDS[1]}}" in
         appoint|model|ceiling|nominate|recommend) words="{offices}" ;;
         recruit) words="{departments}" ;;
         reform) words="{modes}" ;;
         completions) words="bash zsh fish" ;;
         apply) words="--skip --commit" ;;
         {json_cases}) words="--json" ;;
       esac ;;
  esac
  COMPREPLY=($(compgen -W "$words" -- "$cur"))
  # bash 在冒号处分词，冒号前的部分已视为另一个词，补全结果中去掉；zsh（bashcompinit）不分，原样给出。
  local colon="${{cur%"${{cur##*:}}"}}"
  [ -n "$colon" ] && [[ "$COMP_WORDBREAKS" == *:* ]] && COMPREPLY=("${{COMPREPLY[@]#"$colon"}}")
}}
complete -F _ziyuan ziyuan
"#,
        json_cases = JSON_COMMANDS.join("|")
    );
    match shell {
        Shell::Bash => bash,
        // zsh 借 bashcompinit 复用同一份脚本。
        Shell::Zsh => format!("autoload -U +X bashcompinit && bashcompinit\n{bash}"),
        Shell::Fish => {
            let seen =
                |cmds: &str| format!("complete -c ziyuan -n '__fish_seen_subcommand_from {cmds}'");
            [
                "complete -c ziyuan -f".to_owned(),
                format!("complete -c ziyuan -n __fish_use_subcommand -a '{commands}'"),
                format!(
                    "{} -a '{offices}'",
                    seen("appoint model ceiling nominate recommend")
                ),
                format!("{} -a '{departments}'", seen("recruit")),
                format!("{} -a '{modes}'", seen("reform")),
                format!("{} -a 'bash zsh fish'", seen("completions")),
                format!("{} -l skip -x", seen("apply")),
                format!("{} -l commit", seen("apply")),
                format!("{} -l json", seen(&json)),
                format!(
                    "{} -l dept -x -a '{departments}'",
                    seen("edict post template")
                ),
                format!(
                    "{} -l tools -x -a '{tools} none'",
                    seen("edict template ceiling github")
                ),
            ]
            .join("\n")
                + "\n"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::parse;

    #[test]
    fn every_completed_subcommand_is_recognised() {
        for name in SUBCOMMANDS {
            if let Err(e) = parse(vec![name.to_string()]) {
                assert!(!e.to_string().contains("未知命令"), "{name}: {e}");
            }
        }
        let script = completions(Shell::Bash);
        assert!(script.contains("minister:works") && script.contains("write_file"));
    }
}
