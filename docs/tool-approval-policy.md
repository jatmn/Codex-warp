# Tool Approval Policy

Codex Warp can apply an opt-in policy layer to downstream tool calls before
Codex executes them. The current implementation focuses on GitHub CLI approval
hints and token-disclosure blocking.

The goal is not to bypass Codex's sandbox or approval system. Codex still owns
the final execution decision. The policy layer normalizes obvious safe approval
requests, avoids suggesting reusable approval prefixes for complex shell
commands, and can block commands that would expose credentials.

**Notice:** tool approval policy changes the approval metadata Codex receives
for tool calls. Misconfigured rules can approve too much, prompt too little, or
block commands you expected to run. Review every rule before enabling it. You
are responsible for your own configuration and use this feature at your own
risk.

Enable the GitHub policy rules with:

```toml
[tool_policy]
enabled = true
mode = "assist"

[config]
tool_policy_include = ["configs/tool-policies/github.toml"]
```

Modes:

| Mode | Behavior |
| --- | --- |
| `observe` | Classify only; leave tool calls unchanged. |
| `assist` | Add approval metadata for `allow_hint` and `force_manual`; block `deny` matches before Codex sees the tool call. |
| `enforce` | Add approval metadata and block `deny` matches before Codex sees the tool call. |

## Policy Outcomes

| Outcome | Meaning | Codex Warp behavior |
| --- | --- | --- |
| `allow_hint` | The tool call is simple enough to decorate with an approval hint. | Add or normalize `sandbox_permissions`, `prefix_rule`, and `justification`. |
| `manual` | The tool call may be valid, but it is too expressive for reusable approval. | Leave the command intact and do not add a reusable `prefix_rule`. |
| `force_manual` | The tool call should not receive a reusable approval hint, even if it may be valid. | Add escalation metadata without a reusable allow prefix. |
| `deny` | The tool call matches a known forbidden action. | Block the tool call and return a policy error before Codex executes it. |

`deny` wins over all other outcomes. `force_manual` wins over `allow_hint`, and
`manual` wins over `allow_hint` whenever a command has shell features that make
reusable approval unsafe.

Codex's guardian auto-review currently returns only allow/deny. Codex's command
execution policy path separately supports user approval prompts through
`NeedsApproval`, and execpolicy rules can use `decision = "prompt"` for commands
that should always be presented to the user. From Warp's Responses translation
layer, `force_manual` can require escalation and withhold a reusable
`prefix_rule`; it cannot guarantee that Codex Desktop will bypass auto-review
and present a human-only prompt.

## Shell Safety Classifier

The policy layer should parse shell commands conservatively before applying
allow hints.

| Check | `allow_hint` behavior | Rationale |
| --- | --- | --- |
| Single simple command segment | Eligible for allow hints. | Reusable approval prefixes are intended for plain argv-like commands. |
| Compound shell operators such as `;`, `&&`, `||`, and pipes | Mark `manual` or `force_manual` depending on the rule. | Valid workflows may use these, but the whole command should be reviewed as written. |
| Redirection, heredocs, command substitution, env assignments, globs, or other shell expansion | Mark `manual` or `force_manual` depending on the rule. | These can change what actually runs or where output goes. |
| Known credential disclosure command | Mark `deny`. | Secrets should not be printed into Codex logs or transcripts. |

## GitHub Policy Table

| Rule | Match | Requirements | Outcome | Prefix hint | Notes |
| --- | --- | --- | --- | --- | --- |
| GitHub auth status | `["gh", "auth", "status"]` | Simple shell | `allow_hint` | `["gh", "auth", "status"]` | Checks whether auth exists without printing the token. |
| GitHub auth login | `["gh", "auth", "login", ...]` | Simple shell | `force_manual` | None | Interactive auth can print a device/browser code while the command is still running, so Warp avoids reusable approval and warns the user to check pending command output. |
| GitHub setup git credentials | `["gh", "auth", "setup-git"]` | Simple shell | `allow_hint` | `["gh", "auth", "setup-git"]` | Wires Git to use the existing GitHub CLI auth. |
| GitHub auth token | `["gh", "auth", "token", ...]` | Any | `deny` | None | Prints the stored GitHub token. |
| GitHub PR reads | `["gh", "pr", "view", ...]`, `["gh", "pr", "diff", ...]`, `["gh", "pr", "list", ...]` | Simple shell | `allow_hint` | `["gh", "pr"]` | Matches Codex's useful reusable prefix shape. |
| GitHub API calls | `["gh", "api", ...]` | Simple shell | `manual` | None | API method and endpoint filtering is not implemented yet, so Warp does not suggest a reusable prefix. |
| GitHub issue reads | `["gh", "issue", "view", ...]`, `["gh", "issue", "list", ...]` | Simple shell | `allow_hint` | `["gh", "issue"]` | Same shape as existing Codex approvals. |
| GitHub checkout/write operations | `["gh", "pr", "checkout", ...]`, `["gh", "repo", "clone", ...]` | Simple shell | `force_manual` | None | Useful, but writes files and may fetch untrusted code. |
| Compound GitHub command | Any allowed GitHub command plus shell operators or redirection | Any | `force_manual` | None | The command may be legitimate, but should be reviewed as a full shell expression. |

## TOML Shape

```toml
[tool_policy]
enabled = false
mode = "assist" # observe | assist | enforce

[config]
tool_policy_include = ["configs/tool-policies/github.toml"]

[[tool_policy.rules]]
id = "github_pr_read"
tool_name = "shell_command"
match_kind = "command_prefix"
command_prefix = ["gh", "pr", "view"]
shell = "simple"
outcome = "allow_hint"
reason = "github_read_or_auth"
prefix_rule = ["gh", "pr"]
justification = "GitHub access is needed for the requested Codex task. Do you want to allow this command?"
```

The default config includes `configs/tool-policies/github.toml`, but the policy
layer stays inactive until `[tool_policy].enabled = true`. Additional policy
includes append rules. To replace the bundled rules, set
`tool_policy_replace = true` in the config layer that declares your replacement
rules or replacement includes:

```toml
[config]
tool_policy_replace = true
tool_policy_include = ["configs/tool-policies/custom.toml"]
```

Supported rule fields:

| Field | Meaning |
| --- | --- |
| `id` | Stable rule name for diagnostics. |
| `enabled` | Optional; defaults to `true`. |
| `tool_name` | Tool function name, currently usually `shell_command`. |
| `match_kind` | `command_prefix`, `github_auth_token`, or `any`. |
| `command_prefix` | argv prefix for `command_prefix` rules. |
| `shell` | `simple`, `complex`, or `any`. |
| `outcome` | `allow_hint`, `manual`, `force_manual`, or `deny`. |
| `reason` | Machine-readable decision reason. |
| `prefix_rule` | Optional reusable approval prefix for `allow_hint`. |
| `justification` | Optional approval prompt text. |

## Current Limits

- `assist` and `enforce` add escalation metadata for `allow_hint` and
  `force_manual`, and both block `deny` decisions. Plain `manual` decisions are
  left unchanged.
- `force_manual` does not install Codex execpolicy `prompt` rules. It only
  prevents Warp from suggesting a reusable `prefix_rule`.
- The policy does not run synchronous authentication checks such as
  `gh auth status`.
- GitHub API path filtering is not implemented yet; `gh api` calls are
  classified as `manual` and do not receive reusable approval hints.
- Policy decisions are observable through rewritten tool-call arguments when
  debug body logging is enabled, but there is no separate policy event log yet.
