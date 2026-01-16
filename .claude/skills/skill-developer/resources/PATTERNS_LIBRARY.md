# Common Patterns Library

Ready-to-use regex and glob patterns for skill triggers. Copy and customize for your skills.

---

## Intent Patterns (Regex)

### Feature/Endpoint Creation
```regex
(add|create|implement|build).*?(feature|endpoint|route|service|controller)
```

### Component Creation
```regex
(create|add|make|build).*?(component|UI|page|modal|dialog|form)
```

### Blockchain Work
```regex
(add|create|modify|implement).*?(consensus|block|vote|order|certificate)
(hotstuff|orderbook).*?(change|update|implement)
```

### Error Handling
```regex
(fix|handle|catch|debug).*?(error|exception|bug)
(add|implement).*?(try|catch|error.*?handling)
```

### Explanation Requests
```regex
(how does|how do|explain|what is|describe|tell me about).*?
```

### Testing
```regex
(write|create|add).*?(test|spec|unit.*?test)
```

---

## File Path Patterns (Glob)

### Rust Backend
```glob
src/**/*.rs              # All Rust files
src/consensus/**/*.rs    # Consensus module only
src/app/**/*.rs          # Application layer
src/crypto/**/*.rs       # Cryptography
src/api/**/*.rs          # API layer
```

### Next.js Frontend
```glob
web/**/*.tsx             # All React components
web/**/*.ts              # All TypeScript files
web/components/**        # Only components directory
web/lib/**/*.ts          # Library utilities
web/app/**/*.tsx         # App router pages
```

### Test Exclusions
```glob
**/*.test.ts             # TypeScript tests
**/*.test.tsx            # React component tests
**/*.spec.ts             # Spec files
**/tests/**              # Test directories
```

---

## Hyperlicked-Specific Patterns

### Blockchain Skill Triggers
```json
{
  "promptTriggers": {
    "keywords": [
      "consensus", "hotstuff", "pacemaker", "safety",
      "block", "vote", "certificate", "QC",
      "orderbook", "matching", "fill", "order",
      "mempool", "transaction",
      "BLS", "signature", "EIP-712",
      "AppHook", "BlockStore"
    ],
    "intentPatterns": [
      "(create|add|modify).*?(consensus|block|vote|order)",
      "(how|what).*?(consensus|orderbook|matching)",
      "rust.*?(pattern|convention)"
    ]
  },
  "fileTriggers": {
    "pathPatterns": ["src/**/*.rs"]
  }
}
```

### Frontend Skill Triggers
```json
{
  "promptTriggers": {
    "keywords": [
      "component", "zustand", "tailwind",
      "websocket", "wallet", "next.js",
      "trading UI", "orderbook UI", "toast",
      "useWallet", "useWebSocket"
    ],
    "intentPatterns": [
      "(create|add|modify).*?(component|page|hook)",
      "(how|what).*?(zustand|tailwind|wallet)",
      "frontend.*?(pattern|convention)"
    ]
  },
  "fileTriggers": {
    "pathPatterns": ["web/**/*.tsx", "web/**/*.ts"]
  }
}
```

---

## Usage Example

```json
{
  "my-skill": {
    "type": "domain",
    "enforcement": "suggest",
    "priority": "high",
    "promptTriggers": {
      "keywords": ["specific-term"],
      "intentPatterns": [
        "(create|add|build).*?(thing|other)"
      ]
    },
    "fileTriggers": {
      "pathPatterns": [
        "path/to/files/**/*.ext"
      ]
    }
  }
}
```

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [TRIGGER_TYPES.md](TRIGGER_TYPES.md) - Detailed trigger documentation
- [SKILL_RULES_REFERENCE.md](SKILL_RULES_REFERENCE.md) - Complete schema
