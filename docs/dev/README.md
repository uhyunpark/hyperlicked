# Dev Docs

Context persistence files for AI-assisted development. This directory survives context resets.

## Purpose

When working on complex features that span multiple sessions, create context files here to maintain continuity.

## Structure

```
docs/dev/
├── README.md                    # This file
├── {task-name}-plan.md          # Strategic plan (goals, architecture decisions)
├── {task-name}-context.md       # Key files, decisions, current state
└── {task-name}-tasks.md         # Actionable checklist
```

## Usage

### Starting a New Task

1. Read `CLAUDE.md` for project overview
2. Check `docs/blockchain/ROADMAP.md` for priorities
3. Check this directory for active task context
4. Create context files if starting a multi-session task

### Context File Templates

**{task-name}-plan.md**:
```markdown
# {Task Name} Plan

## Goal
What we're building and why.

## Architecture Decisions
- Decision 1: Reasoning
- Decision 2: Reasoning

## Approach
High-level implementation strategy.
```

**{task-name}-context.md**:
```markdown
# {Task Name} Context

## Key Files
- `path/to/file.rs` - Brief description
- `path/to/other.ts` - Brief description

## Current State
Where we left off, what's working.

## Blockers
Any issues or questions that need resolution.
```

**{task-name}-tasks.md**:
```markdown
# {Task Name} Tasks

## Completed
- [x] Task 1
- [x] Task 2

## In Progress
- [ ] Task 3

## Pending
- [ ] Task 4
- [ ] Task 5
```

## Guidelines

- **Keep files under 500 lines** - Matches codebase rule
- **Update frequently** - Stale context is worse than no context
- **Be specific** - Include file paths and line numbers
- **Clean up** - Move completed task docs to `docs/features/{feature}/` when done

## Related

- `docs/blockchain/ROADMAP.md` - Backend priorities
- `docs/frontend/` - Frontend architecture docs
- `docs/plans/` - Implementation plans for specific features
- `docs/features/` - Completed feature documentation
