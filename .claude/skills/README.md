# Claude Code Skills

Auto-activated skills for the hyperlicked project. Skills suggest themselves based on your prompts and file patterns.

## Available Skills

| Skill | Type | Purpose |
|-------|------|---------|
| `skill-developer` | Meta | Create and manage Claude Code skills |
| `blockchain-dev-guidelines` | Domain | Rust/HotStuff-2 consensus and orderbook patterns |
| `frontend-dev-guidelines` | Domain | Next.js 15/Tailwind/Zustand frontend patterns |

## How Skills Work

1. **UserPromptSubmit Hook** - When you type a prompt, the hook checks for keyword/intent matches
2. **Skill Suggestion** - If matched, you'll see a suggestion to use the Skill tool
3. **Progressive Disclosure** - Each skill has a main SKILL.md (<500 lines) and resource files for details

## Configuration

Skills are configured in `skill-rules.json`:
- `promptTriggers.keywords` - Explicit topic matches
- `promptTriggers.intentPatterns` - Regex for implicit actions
- `fileTriggers.pathPatterns` - Glob patterns for file-based activation

## Adding New Skills

Use the `skill-developer` skill to create new skills. It provides:
- Skill structure templates
- Trigger pattern guidance
- Testing instructions
- Best practices

## Skill Structure

```
.claude/skills/
├── README.md                           # This file
├── skill-rules.json                    # Trigger configuration
└── {skill-name}/
    ├── SKILL.md                        # Main file (<500 lines)
    └── resources/                      # Detailed reference files
        ├── TOPIC1.md                   # Deep dive on topic 1
        └── TOPIC2.md                   # Deep dive on topic 2
```

## 500-Line Rule

Each file should stay under 500 lines:
- Keep SKILL.md as a concise overview
- Use resources/ for detailed documentation
- Claude loads resources only when needed (progressive disclosure)
