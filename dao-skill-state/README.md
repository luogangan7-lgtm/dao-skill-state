# 道 · Skill.state

独立维护的 Skill 创建与改进工具。版本 `0.3.0`，机器标识 `dao-skill-state`。

把需求变成小而可用的技能：先发现复用，再创建，最后验证。简单任务不加载长流程；长任务可按需加入状态契约。它不是社媒运营百科、插件平台，也不是已经实现的 SKILL.state 运行时。

## 使用

让支持本地 Skill 的 agent 读取本目录 `SKILL.md`，或在宿主已发现本包后调用：

```text
使用 $dao-skill-state，做一个把商品卖点整理成主图设计简报的 Skill，只生成简报，不生成图片、不发布。
```

创建和规则阅读不需要网络服务或私有 MCP。格式检查需要 Python 3.9+ 和 PyYAML；请使用已有可信环境，缺依赖时明确报告，不能自动安装。可在需要时审阅 `requirements.txt` 后由维护者安装。

```bash
python3 scripts/check_skill.py .
python3 -m unittest discover -s scripts -p 'test_*.py' -v
```

检查器只校验 frontmatter，不解析或认证完整 Markdown 链接、正文质量或业务效果。区分 errors 与 warnings：0=结构有效，1=结构错误，2=无法检查（包括依赖/读取失败或超过本工具 1 MiB 输入限制）。具体能力以当前验收记录为准。

## 内容与维护

- `SKILL.md`：短入口、任务路由、权限与交付合同。
- `references/authoring.md`：创建、拆分组合与生成物验证。
- `references/review.md`：测量、E0–E4、Trust 和证据驱动改进。
- `references/assessment.md`：仅评分/验收时加载；任务结果、对照增益、成本与覆盖分开，八维不再加权成通用质量分。
- `references/evolution.md`：实际反馈→候选消费→独立接纳→可恢复本地更新；可选 stdlib helper 的 PASS 仅为机械绑定。
- `references/forms.md`：创建、演进或审查时判断 fixed steps 还是 stateful，以及 stateful 必备三件事。
- `references/state-contract.md`：可选长任务续接、语义校验、来源时效与动作回执。
- `scripts/`：只读格式检查、可选任务记录计数检查和回归测试；计数检查不验证证据内容或授权发布。

一个维护源，不把运行状态、用户资料或测试转录写进包内。按真实缺陷、标准变动或明确需求更新；记录版本、文件摘要、失败用例、验收结果和回滚点。新版本先在隔离目录验证，再按用户授权替换安装。不要强制 Git 远程或私有任务系统；也不自动提交、发布或推送。

本地发现使用宿主支持的目录或符号链接。符号链接可保持单一源码，但源盘离线时技能也不可用。本次作为旧 dao-skill 的替代包保留 explicit-only 策略：`allow_implicit_invocation: false`；用 `$dao-skill-state` 明确调用，不得因更名静默开启隐式调用。

## 来源与许可

独立维护不等于冒称全部原创。沿用与改写的 dao-skill 方法保留 MIT 归属，完整说明见 [NOTICE.md](NOTICE.md) 和 [LICENSE](LICENSE)。没有原仓远程依赖，没有自动同步上游，也未声称由论文作者背书。
