# Model-Family Catalogs

Model-family catalogs teach Codex Warp about model behavior independent of the
gateway that serves the model. Use them for context windows, modalities,
reasoning shapes, search support, and tool quirks that belong to the model or
model brand.

Provider/gateway behavior belongs in
[provider catalogs](provider-catalogs.md).

## Table Of Contents

- [Included Catalogs](#included-catalogs)
- [Current Exact Model Coverage](#current-exact-model-coverage)
- [Matching And Priority](#matching-and-priority)
- [What Belongs In A Model Catalog](#what-belongs-in-a-model-catalog)
- [Metadata Fields](#metadata-fields)
- [Transform Fields](#transform-fields)
- [Common Transform Patterns](#common-transform-patterns)
- [Adding A New Model Brand](#adding-a-new-model-brand)
- [Adding A New Model To An Existing Brand](#adding-a-new-model-to-an-existing-brand)
- [Review Checklist](#review-checklist)

## Included Catalogs

| Parent brand | Catalog | Examples |
| --- | --- | --- |
| DeepSeek | [`deepseek.toml`](../configs/model-families/deepseek.toml) | `deepseek-v3.2`, `deepseek-v3.2-speciale`, `deepseek-v4-flash`, `deepseek-v4-pro` |
| MiniMax | [`minimax.toml`](../configs/model-families/minimax.toml) | `minimax-m2.5`, `minimax-m2.7`, `minimax-m3` |
| Moonshot AI | [`moonshot-ai.toml`](../configs/model-families/moonshot-ai.toml) | `kimi-k2`, `kimi-k2-0905`, `kimi-k2.5`, `kimi-k2.6`, `kimi-k2.6-code`, `kimi-k2.7-code`, `kimi-k2.7-code-highspeed`, `kimi-for-coding` |
| Alibaba Cloud | [`qwen.toml`](../configs/model-families/qwen.toml) | `qwen3.6-35b-a3b`; conservative broad defaults for `qwen3.6*` and `qwen3.7*` |
| xAI | [`x-ai.toml`](../configs/model-families/x-ai.toml) | `grok-4.3`, `grok-4.5`, `grok-build-0.1` |
| Xiaomi | [`xiaomi.toml`](../configs/model-families/xiaomi.toml) | `mimo-v2.5`, `mimo-v2.5-pro` |
| Z.ai | [`z-ai.toml`](../configs/model-families/z-ai.toml) | `glm-5`, `glm-5.1`, `glm-5.2` |

The default catalogs live in
[`configs/model-families`](../configs/model-families) and are loaded by
[`codex-warp.toml`](../codex-warp.toml).

## Current Exact Model Coverage

Broad family entries should contain only behavior shared by every matched model,
which may include tool defaults or genuinely shared model behavior. Exact
entries below carry model-specific context, modality, reasoning, search, or
transform behavior.

| Brand | Exact entry | Context | Modalities | Reasoning | Search | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| DeepSeek | `deepseek-v3.2` | 128k | provider/default | low, medium, high; default medium | provider/default | Parallel tool metadata is true, but requests force `parallel_tool_calls = false`. |
| DeepSeek | `deepseek-v3.2-speciale` | 128k | provider/default | low, medium, high; default high | provider/default | Forces `parallel_tool_calls = false`. |
| DeepSeek | `deepseek-v4-flash` | 1,000k | provider/default | low, medium, high; default medium | provider/default | Converts `reasoning.effort` to `thinking.type` and forwards `reasoning_effort`; preserves reasoning history (`preserve_reasoning_content_history`); forces `parallel_tool_calls = false`. |
| DeepSeek | `deepseek-v4-pro` | 1,000k | provider/default | low, medium, high; default high | provider/default | Converts `reasoning.effort` to `thinking.type` and forwards `reasoning_effort`; preserves reasoning history (`preserve_reasoning_content_history`); forces `parallel_tool_calls = false`. |
| MiniMax | `minimax-m2.5` | 192k | text | none | true | Non-reasoning variant; forces `parallel_tool_calls = false`. |
| MiniMax | `minimax-m2.7` | 200k | text | low, medium, high; default high | true | Converts `reasoning.effort` to `thinking.type`; forces `parallel_tool_calls = false`. |
| MiniMax | `minimax-m3` | 1,000k | text, image | low, medium, high; default high | false | Converts `reasoning.effort` to `thinking.type`; forces `parallel_tool_calls = false`. |
| Moonshot AI | `kimi-k2`, `kimi-k2-instruct`, `kimi-k2-0711` | 128k | text | none | provider/default | Forces `parallel_tool_calls = false`. |
| Moonshot AI | `kimi-k2-0905` | 256k | text | none | provider/default | Forces `parallel_tool_calls = false`. |
| Moonshot AI | `kimi-k2.5` | usable 220k, max 262,144 | text, image, video | none, low, medium, high; default medium | true | Uses a conservative Codex planning window below the provider 256K cap to leave output/tokenizer headroom; converts `reasoning.effort` to `thinking.type`; forces `parallel_tool_calls = false`. |
| Moonshot AI | `kimi-k2.6` | usable 220k, max 262,144 | text, image, video | none, low, medium, high; default medium | true | Uses a conservative Codex planning window below the provider 256K cap to leave output/tokenizer headroom; converts `reasoning.effort` to `thinking.type`; forces `parallel_tool_calls = false`. |
| Moonshot AI | `kimi-k2.6-code` | usable 220k, max 262,144 | text, image | high | provider/default | Uses a conservative Codex planning window below the provider 256K cap to leave output/tokenizer headroom; sets `thinking.type = enabled` and `thinking.keep = all`; forces `parallel_tool_calls = false`. |
| Moonshot AI | `kimi-k2.7-code`, `kimi-k2.7-code-highspeed`, `kimi-for-coding` | usable 220k, max 262,144 | text, image | high | provider/default | Uses a conservative Codex planning window below the provider 256K cap to leave output/tokenizer headroom; sets `thinking.type = enabled` and `thinking.keep = all`; drops K2.7-rejected sampling overrides; forces `parallel_tool_calls = false`. |
| Alibaba Cloud | `qwen3.6-35b-a3b` | 262k, max 1,010k | text, image, video | high | false | Removes inherited OpenAI `reasoning_effort`; broad `qwen3.6*` and `qwen3.7*` entries also drop it until a gateway documents support. |
| xAI | `grok-4.3` | 1,000k | text, image | low, medium, high; default medium | true | Uses `web_search`; parallel tool metadata is true, but requests force `parallel_tool_calls = false`. |
| xAI | `grok-4.5` | 500k | text, image | low, medium, high; default high | true | Uses `web_search`; reasoning cannot be disabled (`none` mapped to `low` via `reasoning_effort_none_value`); parallel tool metadata is true, but requests force `parallel_tool_calls = false`. |
| xAI | `grok-build-0.1` | 256k | text | low, medium, high; default medium | true | Uses `web_search`; parallel tool metadata is true, but requests force `parallel_tool_calls = false`. |
| Xiaomi | `mimo-v2.5` | 1,000k | text, image | low, medium, high; default medium | provider/default | Gateway profile does not carry model-specific overrides. |
| Xiaomi | `mimo-v2.5-pro` | 1,000k | text | low, medium, high; default medium | provider/default | Gateway profile does not carry model-specific overrides. |
| Z.ai | `glm-5`, `glm-5.1` family | 200k | text | low, medium, high; default medium | provider/default | Broad GLM-5 transform converts `reasoning.effort` to `thinking.type`; forces `parallel_tool_calls = false`. |
| Z.ai | `glm-5.2` | 1,000k | text | low, medium, high; default medium | provider/default | Inherits broad GLM-5 transform; forwards `reasoning_effort` alongside `thinking.type` (GLM-5.2 honors it natively). |

## Matching And Priority

Each catalog entry lives under `model_families.<id>`:

```toml
[model_families.example_family]
priority = 0
patterns = ["example-*"]
```

Patterns are case-insensitive and support `*` wildcards. Matching entries are
applied in ascending `priority`; ties are sorted by entry id. This lets a broad
family entry apply shared behavior first, then exact model entries override it.

Recommended priority convention:

| Priority | Use |
| --- | --- |
| `0` | Broad family or brand defaults. |
| `10` | Exact model or current variant overrides. |
| `20` | Emergency/special-case overrides that must win over normal exact models. |

## What Belongs In A Model Catalog

Put behavior here when it follows the model across providers:

- context window
- input modalities
- reasoning support and default effort
- search support
- parallel tool-call support
- Codex tool metadata such as `shell_type` and `apply_patch_tool_type`
- model-specific request morphs such as `reasoning.effort -> thinking.type`

Do not put gateway-specific details here:

- provider base URLs
- API keys
- auth headers
- provider-required static headers
- endpoint paths
- a gateway-specific correction for one provider's broken `/models` response

## Metadata Fields

Supported model metadata fields include:

| Field | Meaning |
| --- | --- |
| `context_window` | Main usable context window. Also updates the truncation policy. |
| `max_context_window` | Provider-reported maximum context, when distinct. |
| `auto_compact_token_limit` | Codex auto-compact threshold. |
| `comp_hash` | Codex compaction compatibility hash for model-switch compaction. |
| `effective_context_window_percent` | Percent of the context window Codex should treat as usable. |
| `input_modalities` | Usually `["text"]` or `["text", "image"]`. |
| `supports_image_detail_original` | Whether original image detail is supported. |
| `supports_parallel_tool_calls` | Whether the model can handle parallel tool calls. |
| `supports_search_tool` | Whether a search tool is supported. |
| `supports_reasoning_summaries` | Whether reasoning summaries are available. |
| `support_verbosity` | Whether verbosity controls are supported. |
| `supported_reasoning_levels` | Reasoning efforts such as `none`, `low`, `medium`, `high`. |
| `default_reasoning_level` | Default effort Codex should show/use. |
| `default_reasoning_summary` | Default reasoning summary mode. |
| `include_skills_usage_instructions` | Whether Codex should include skill usage instructions in model context. |
| `apply_patch_tool_type` | Codex apply-patch tool type, often `freeform`. |
| `shell_type` | Codex shell tool type, often `shell_command`. |
| `web_search_tool_type` | Search tool type exposed to Codex. |
| `experimental_supported_tools` | Extra Codex experimental tools the model may use. |
| `use_responses_lite` | Whether the model should use Responses Lite behavior. |
| `auto_review_model_override` | Optional family-local model for Codex auto-review turns. Use the unprefixed target model id; provider catalogs localize it to their own matching id. |
| `tool_mode` | Optional Codex tool mode hint. |
| `multi_agent_version` | Optional multi-agent runtime selector such as `v1`, `v2`, or `disabled`. |

Example:

```toml
[model_families.example_model.model_metadata]
context_window = 200000
default_reasoning_level = "medium"
supported_reasoning_levels = ["low", "medium", "high"]
input_modalities = ["text", "image"]
supports_parallel_tool_calls = false
shell_type = "shell_command"
apply_patch_tool_type = "freeform"
include_skills_usage_instructions = true
```

Codex currently accepts only `text` and `image` in `/models` metadata. Catalogs
may retain upstream notes such as video support for documentation, but Codex
Warp filters unsupported modality values out of the served model catalog.

## Transform Fields

Transforms let a model entry override or adjust request translation.

| Field | Meaning |
| --- | --- |
| `backend` | `open_ai_chat` or `responses`. |
| `chat_request_morphs` | Replace all inherited chat request morphs. |
| `responses_request_morphs` | Replace all inherited native Responses morphs. |
| `remove_chat_request_morphs` | Remove matching inherited chat morphs. |
| `remove_responses_request_morphs` | Remove matching inherited Responses morphs. |
| `append_chat_request_morphs` | Append chat morphs after removals. |
| `append_responses_request_morphs` | Append native Responses morphs after removals. |
| `unsupported_tool_types` | Tool types to rewrite, drop, or pass through. |
| `unsupported_tool_strategy` | `drop`, `as_function`, or `passthrough`. |
| `drop_empty_tool_choice` | Whether to avoid forwarding empty/default tool choice. |
| `force_parallel_tool_calls` | Force `parallel_tool_calls` to a boolean value. |
| `request_stream_options_include_usage` | Add `stream_options.include_usage = true` for streamed chat requests when the provider documents support and the caller did not set `stream_options`. |

Supported morph kinds:

| Kind | Meaning |
| --- | --- |
| `copy` | Copy a request path. |
| `rename` | Copy to a target path; native Responses morphs also remove the source. |
| `drop` | Discard a path. |
| `text_format` | Convert Responses `text.format` JSON schema to chat `response_format`. |
| `thinking_type` | Convert `reasoning.effort` into provider `thinking.type`. |
| `static_string` | Set a fixed string value. |

## Common Transform Patterns

Replace OpenAI-style reasoning with provider `thinking.type`:

```toml
[[model_families.example_model.transform.remove_chat_request_morphs]]
from = "reasoning.effort"
to = "reasoning_effort"
kind = "rename"

[[model_families.example_model.transform.append_chat_request_morphs]]
from = "reasoning.effort"
to = "thinking.type"
kind = "thinking_type"
```

Set fixed fields for always-thinking models:

```toml
[[model_families.example_code_model.transform.append_chat_request_morphs]]
from = ""
to = "thinking.type"
value = "enabled"
kind = "static_string"

[[model_families.example_code_model.transform.append_chat_request_morphs]]
from = ""
to = "thinking.keep"
value = "all"
kind = "static_string"
```

Convert Codex custom/freeform tools into ordinary functions:

```toml
[model_families.example_model.transform]
unsupported_tool_types = ["custom"]
unsupported_tool_strategy = "as_function"
```

Disable parallel tool calls for models that reject them:

```toml
[model_families.example_model.transform]
force_parallel_tool_calls = false
```

## Adding A New Model Brand

1. Create `configs/model-families/<brand>.toml`.
2. Add one broad entry for shared brand behavior.
3. Add exact entries for each model or variant with different context,
   modality, reasoning, or tool behavior.
4. Use `priority = 0` for broad entries and `priority = 10` for exact entries.
5. Add the new file to the `model_family_include` list under `[config]` in
   `codex-warp.toml`.
6. Add the brand to the README supported model-family table.
7. Add tests that prove:
   - the file loads,
   - exact models match,
   - context/modality/reasoning metadata is correct,
   - request transforms do not leak incompatible inherited fields.

## Adding A New Model To An Existing Brand

1. Open the brand catalog.
2. Check whether the new model actually shares the broad family behavior.
3. If it has different specs, add a new exact entry instead of expanding a broad
   pattern.
4. Include common aliases, for example dotted and dashed names:

   ```toml
   patterns = [
     "example-v2.5",
     "example_v2.5",
     "example-v2-5",
     "example_v2_5",
   ]
   ```

5. Add or update tests for the new model id.

## Review Checklist

Before committing catalog changes:

- Do broad entries contain only genuinely shared behavior?
- Are context windows model-specific where needed?
- Are reasoning morphs removing incompatible inherited morphs first?
- Are modality and search flags model-specific?
- Do provider-specific auth, headers, and endpoints stay out of model catalogs?
- Does `cargo test` cover the new catalog behavior?
