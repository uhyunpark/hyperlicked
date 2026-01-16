# Commit

Analyze changes and create logical, well-organized git commits.

## Your Task

1. **Analyze all changes**:
   ```bash
   git status
   git diff
   git diff --cached
   ```

2. **Group changes into logical commits**:

   Changes should be grouped by:
   - **Feature**: Related files that implement one feature together
   - **Layer**: Same architectural layer (consensus, network, app, api)
   - **Purpose**: Same type of change (refactor, bugfix, tests, docs)

   Example groupings:
   - `src/consensus/engine.rs` + `src/consensus/types.rs` → one commit
   - `src/consensus/` + `tests/consensus_test.rs` → one commit
   - `CLAUDE.md` alone → separate docs commit
   - `.claude/hooks/*` + `.claude/commands/*` → one tooling commit

3. **For each logical group**:

   a. Stage only the files in that group:
   ```bash
   git add <file1> <file2> ...
   ```

   b. Write a descriptive commit message:
   ```
   <type>(<scope>): <short summary>

   <optional body explaining WHY, not WHAT>
   ```

   Types: `feat`, `fix`, `refactor`, `test`, `docs`, `chore`, `build`

   c. Create the commit:
   ```bash
   git commit -m "<message>"
   ```

4. **Verify commits**:
   ```bash
   git log --oneline -5
   ```

## Commit Message Guidelines

**Good commit messages**:
```
feat(consensus): implement HotStuff-2 voting rules

Add safety module with vote verification and QC formation.
Follows 2-chain commit rule from HotStuff-2 paper.
```

```
refactor(types): extract Block and Vote into separate files

Improves modularity and keeps files under 500 LOC limit.
```

**Bad commit messages**:
```
update files          # Too vague
fix stuff             # Not descriptive
WIP                   # Not a complete unit
```

## Rules

- **Atomic commits**: Each commit should be one logical change
- **Buildable**: Each commit should pass `cargo build` (don't break the build)
- **No WIP**: Don't commit incomplete work
- **Order matters**: Commit foundational changes before dependent ones

## Special Cases

- **Single small change**: Just make one commit, don't over-split
- **Large refactor**: It's OK to have one big commit if it's truly atomic
- **Docs only**: Can be combined with related code OR separate (your judgment)
