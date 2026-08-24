# Agent skills

Four skills ship in [`.claude/skills/`](../.claude/skills/). They teach a
coding agent when to consult `brake` and — more importantly — how to read the
answer without over-claiming.

They are plain Markdown with YAML frontmatter, so they work in Claude Code
directly and are readable by anything else that consumes the same format.

| Skill | Triggers on | What it does |
| --- | --- | --- |
| [`api-compatibility`](../.claude/skills/api-compatibility/SKILL.md) | Editing or reviewing an OpenAPI, protobuf or GraphQL contract; "will this break clients?" | Checks the change *before* it is written, via MCP or the CLI, and presents the costed ways out rather than picking one |
| [`brake-consumer-impact`](../.claude/skills/brake-consumer-impact/SKILL.md) | "Who uses this endpoint?"; before deleting or renaming anything | Runs the demand inventory and insists that an empty answer means *undeclared*, not unused |
| [`brake-triage`](../.claude/skills/brake-triage/SKILL.md) | A hook or CI job failed on a `brake` finding | Separates "the API broke" (exit `1`) from "the gate broke" (exit `2`), then works the right fix |
| [`brake-adopt`](../.claude/skills/brake-adopt/SKILL.md) | "Add API compatibility checking to this repo" | `init`, baselines, level, hook, CI jobs — in the order that produces a gate people keep |

## Installing them in your own repository

```sh
mkdir -p .claude/skills
curl -sSfL https://github.com/grahambrooks/brake/archive/refs/heads/main.tar.gz \
  | tar -xz --strip-components=3 -C .claude/skills \
      brake-main/.claude/skills
```

Or copy the four directories out of a checkout. They are self-contained: each
`SKILL.md` is the whole skill, with no scripts and no shared includes.

For skills available in every repository rather than one, put them under
`~/.claude/skills/` instead.

## What they encode

The skills are not a wrapper around the CLI — an agent can read `--help`. They
carry the things an agent gets wrong when it works out `brake` for itself:

- **`unavailable` is not clean.** A construct `brake` cannot model is named, and
  rounding it down to a pass manufactures exactly the confidence the tool exists
  to refuse.
- **An empty consumer answer means nobody *declared* it.** Not that nobody uses
  it. Every skill that can produce that answer says so at the point it is
  produced.
- **Exit `2` is not exit `1`.** A broken gate and a broken API need different
  responses, and an agent that conflates them fixes the wrong file.
- **`brake` does not choose between the ways out, so the agent should not
  either** — silently. The strategies come with costs; which one fits depends on
  whether the team controls every consumer and whether there is a version
  scheme, and neither is visible in the repository.
- **A suppression is a dated, reasoned decision, never a way to get a build
  green.** The skills refuse to add one without saying so.

## Pairing them with the MCP server

The skills prefer the [MCP tools](mcp.md) when a `brake` server is connected,
because `check_change` takes the proposed document as text — so a draft that has
not been written to disk can still be checked. That is the moment where the
advice is worth most.

```sh
claude mcp add brake -- brake mcp /path/to/your/repo
```

They fall back to the CLI when it is not, and say so rather than guessing at an
answer `brake` would have given.
