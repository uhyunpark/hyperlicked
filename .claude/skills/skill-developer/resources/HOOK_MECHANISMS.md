# Hook Mechanisms - Deep Dive

Technical deep dive into how the UserPromptSubmit hook works.

## Table of Contents

- [UserPromptSubmit Hook Flow](#userpromptsubmit-hook-flow)
- [Exit Code Behavior](#exit-code-behavior)
- [Performance Considerations](#performance-considerations)

---

## UserPromptSubmit Hook Flow

### Execution Sequence

```
User submits prompt
    ↓
.claude/settings.json registers hook
    ↓
skill-activation-prompt.sh executes
    ↓
npx tsx skill-activation-prompt.ts
    ↓
Hook reads stdin (JSON with prompt)
    ↓
Loads skill-rules.json
    ↓
Matches keywords + intent patterns
    ↓
Groups matches by priority (critical → high → medium → low)
    ↓
Outputs formatted message to stdout
    ↓
stdout becomes context for Claude (injected before prompt)
    ↓
Claude sees: [skill suggestion] + user's prompt
```

### Key Points

- **Exit code**: Always 0 (allow)
- **stdout**: → Claude's context (injected as system message)
- **Timing**: Runs BEFORE Claude processes prompt
- **Behavior**: Non-blocking, advisory only
- **Purpose**: Make Claude aware of relevant skills

### Input Format

```json
{
  "session_id": "abc-123",
  "transcript_path": "/path/to/transcript.json",
  "cwd": "/root/git/your-project",
  "permission_mode": "normal",
  "hook_event_name": "UserPromptSubmit",
  "prompt": "how does the consensus work?"
}
```

### Output Format (to stdout)

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
SKILL ACTIVATION CHECK
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

RECOMMENDED SKILLS:
  -> blockchain-dev-guidelines

ACTION: Use Skill tool BEFORE responding
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

Claude sees this output as additional context before processing the user's prompt.

---

## Exit Code Behavior

### Exit Code Reference Table

| Exit Code | stdout | stderr | Effect |
|-----------|--------|--------|--------|
| 0 | → Claude context | → User only | Normal, skill suggestions shown |
| 1+ | → User only | → User only | Error, hook failed |

### Why stdout Matters

The stdout content is injected into Claude's context:

1. Hook outputs skill suggestions to stdout
2. Claude sees suggestions before the user's prompt
3. Claude becomes aware of relevant skills
4. Claude can use the Skill tool to load the skill

---

## Performance Considerations

### Target Metrics

- **UserPromptSubmit**: < 100ms

### Performance Bottlenecks

1. **Loading skill-rules.json** (every execution)
   - Future: Cache in memory
   - Future: Watch for changes, reload only when needed

2. **Regex matching**
   - Intent patterns compilation
   - Future: Lazy compile, cache compiled regexes

### Optimization Strategies

**Reduce patterns:**
- Use more specific patterns (fewer to check)
- Combine similar patterns where possible

**Simplify regex:**
- Fewer alternations (|) = faster
- More specific patterns = faster matching

### Measure Performance

```bash
time echo '{"prompt":"test"}' | npx tsx .claude/hooks/skill-activation-prompt.ts
```

**Target:** < 100ms

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [TROUBLESHOOTING.md](TROUBLESHOOTING.md) - Debug hook issues
- [SKILL_RULES_REFERENCE.md](SKILL_RULES_REFERENCE.md) - Configuration reference
