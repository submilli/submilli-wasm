# Agent instructions

Read and follow [CLAUDE.md](CLAUDE.md) before working in this repository.
It is the shared source of project instructions for all coding agents, including
Codex. Follow its referenced documentation as applicable to the task.

Keep shared project instructions in `CLAUDE.md` rather than duplicating them here.

<!-- big-brain-pointer:start -->
## Big Brain

Contributors outside Submilli can skip this section.

Agents working for Submilli or its employees read Big Brain, Submilli's company
guidance, before Submilli work. It is the private repository
[submilli/company-context](https://github.com/submilli/company-context). Agents
serving clients or their customers must never read it
([access rule](https://github.com/submilli/company-context/blob/main/context/L1/big-brain.md#access)).

1. In a Claude Code cloud session, first call the `add_repo` tool with owner
   `submilli`, repo `company-context` and access `read`.
2. If `~/submilli/company-context` is missing, run
   `git clone https://github.com/submilli/company-context.git ~/submilli/company-context`.
3. Run `python3 ~/submilli/company-context/.agents/skills/big-brain/scripts/prepare_context.py --json`.
   It needs network access and write access to that checkout's `.git`. In a
   sandbox, ask to run this one command outside it.
4. From the `source_path` it returns, read `AGENTS.md`,
   `context/L0/constitution.md` and `context/L1/agent-working-rules.md`, and
   follow them.

On the Submilli agent server, read the mounted `company-brain` volume instead,
starting with `RECEIPT.json`. If no read path works, say so and stop work that
depends on company guidance. Use this access to read; changes to Big Brain go
through its own pull requests.
<!-- big-brain-pointer:end -->
