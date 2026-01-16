# Trigger Types - Complete Guide

Complete reference for configuring skill triggers in Claude Code's skill auto-activation system.

## Table of Contents

- [Keyword Triggers (Explicit)](#keyword-triggers-explicit)
- [Intent Pattern Triggers (Implicit)](#intent-pattern-triggers-implicit)
- [File Path Triggers](#file-path-triggers)
- [Best Practices Summary](#best-practices-summary)

---

## Keyword Triggers (Explicit)

### How It Works

Case-insensitive substring matching in user's prompt.

### Use For

Topic-based activation where user explicitly mentions the subject.

### Configuration

```json
"promptTriggers": {
  "keywords": ["consensus", "orderbook", "component"]
}
```

### Example

- User prompt: "how does the **consensus** work?"
- Matches: "consensus" keyword
- Activates: `blockchain-dev-guidelines`

### Best Practices

- Use specific, unambiguous terms
- Include common variations ("orderbook", "order book", "matching")
- Avoid overly generic words ("system", "work", "create")
- Test with real prompts

---

## Intent Pattern Triggers (Implicit)

### How It Works

Regex pattern matching to detect user's intent even when they don't mention the topic explicitly.

### Use For

Action-based activation where user describes what they want to do rather than the specific topic.

### Configuration

```json
"promptTriggers": {
  "intentPatterns": [
    "(create|add|implement).*?(feature|endpoint)",
    "(how does|explain).*?(consensus|orderbook)"
  ]
}
```

### Examples

**Blockchain Work:**
- User prompt: "add consensus logic"
- Matches: `(add).*?(consensus)`
- Activates: `blockchain-dev-guidelines`

**Component Creation:**
- User prompt: "create a dashboard widget"
- Matches: `(create).*?(component)`
- Activates: `frontend-dev-guidelines`

### Best Practices

- Capture common action verbs: `(create|add|modify|build|implement)`
- Include domain-specific nouns: `(feature|endpoint|component|workflow)`
- Use non-greedy matching: `.*?` instead of `.*`
- Test patterns thoroughly with regex tester (https://regex101.com/)
- Don't make patterns too broad (causes false positives)
- Don't make patterns too specific (causes false negatives)

### Common Pattern Examples

```regex
# Blockchain Work
(add|create|implement).*?(consensus|block|vote|order)

# Explanations
(how does|explain|what is|describe).*?

# Frontend Work
(create|add|make|build).*?(component|UI|page|modal|dialog)

# Error Handling
(fix|handle|catch|debug).*?(error|exception|bug)
```

---

## File Path Triggers

### How It Works

Glob pattern matching against the file path being edited.

### Use For

Domain/area-specific activation based on file location in the project.

### Configuration

```json
"fileTriggers": {
  "pathPatterns": [
    "src/**/*.rs",
    "web/**/*.tsx"
  ],
  "pathExclusions": [
    "**/*.test.ts"
  ]
}
```

### Glob Pattern Syntax

- `**` = Any number of directories (including zero)
- `*` = Any characters within a directory name
- Examples:
  - `src/**/*.rs` = All .rs files in src and subdirs
  - `web/**/*.tsx` = All .tsx files in web and subdirs

### Example

- File being edited: `src/consensus/engine.rs`
- Matches: `src/**/*.rs`
- Activates: `blockchain-dev-guidelines`

### Best Practices

- Be specific to avoid false positives
- Use exclusions for test files: `**/*.test.ts`
- Consider subdirectory structure
- Test patterns with actual file paths

### Common Path Patterns

```glob
# Rust Backend
src/**/*.rs              # All Rust files
src/consensus/**/*.rs    # Consensus module
src/app/**/*.rs          # Application layer

# Next.js Frontend
web/**/*.tsx             # All React components
web/**/*.ts              # All TypeScript files
web/components/**        # Only components directory
web/lib/**/*.ts          # Library utilities
```

---

## Best Practices Summary

### DO:
- Use specific, unambiguous keywords
- Test all patterns with real examples
- Include common variations
- Use non-greedy regex: `.*?`
- Add exclusions for test files
- Make file path patterns narrow and specific

### DON'T:
- Use overly generic keywords ("system", "work")
- Make intent patterns too broad (false positives)
- Make patterns too specific (false negatives)
- Forget to test with regex tester (https://regex101.com/)
- Use greedy regex: `.*` instead of `.*?`
- Match too broadly in file paths

### Testing Your Triggers

```bash
echo '{"session_id":"test","prompt":"your test prompt here"}' | \
  npx tsx .claude/hooks/skill-activation-prompt.ts
```

Expected: Your skill should appear in the output.

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [SKILL_RULES_REFERENCE.md](SKILL_RULES_REFERENCE.md) - Complete skill-rules.json schema
- [PATTERNS_LIBRARY.md](PATTERNS_LIBRARY.md) - Ready-to-use pattern library
