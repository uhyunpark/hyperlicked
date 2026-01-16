# Claude Code Infrastructure Setup for Hyperlicked

## Overview

Set up Claude Code infrastructure with auto-activating skills, modular skill pattern, and dev docs system.

**Components**:
1. Auto-activating skills via UserPromptSubmit hook
2. Three skills (all following 500-line rule with progressive disclosure):
   - `skill-developer` - Meta-skill for creating skills
   - `blockchain-dev-guidelines` - Rust/HotStuff-2 consensus patterns
   - `frontend-dev-guidelines` - Next.js 15/Tailwind/Zustand patterns
3. Dev docs system at `docs/dev/`
4. Fix existing post-work-verify.sh (./rust → ./src)

---

## Phase 1: Foundation (settings.json + Fix Hook)

### Step 1.1: Create settings.json
**Create**: `/.claude/settings.json`
```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/skill-activation-prompt.sh"
          }
        ]
      }
    ]
  }
}
```
**Verify**: `cat .claude/settings.json | jq .`

### Step 1.2: Fix post-work-verify.sh
**Modify**: `/.claude/hooks/post-work-verify.sh`
- Line 15: `RUST_DIR="$PROJECT_DIR/rust"` → `RUST_DIR="$PROJECT_DIR/src"`
- Line 115: Update hint to reference root CLAUDE.md

**Verify**: `grep "RUST_DIR=" .claude/hooks/post-work-verify.sh`

---

## Phase 2: Skill Activation System

### Step 2.1: Create hooks TypeScript infrastructure
**Create**: `/.claude/hooks/package.json`
```json
{
  "name": "claude-hooks",
  "version": "1.0.0",
  "private": true,
  "type": "module",
  "dependencies": {
    "@types/node": "^20.11.0",
    "tsx": "^4.7.0",
    "typescript": "^5.3.3"
  }
}
```

**Create**: `/.claude/hooks/tsconfig.json` (standard ES2022 Node config)

**Verify**: `cd .claude/hooks && npm install`

### Step 2.2: Create skill-activation-prompt hook
**Create**: `/.claude/hooks/skill-activation-prompt.sh`
```bash
#!/bin/bash
set -e
cd "$CLAUDE_PROJECT_DIR/.claude/hooks"
cat | npx tsx skill-activation-prompt.ts
```

**Create**: `/.claude/hooks/skill-activation-prompt.ts`
- Adapt from: `claude-code-practice/.claude/hooks/skill-activation-prompt.ts`
- Loads skill-rules.json, matches keywords/intent patterns, outputs suggestions

**Verify**: `echo '{"session_id":"test","prompt":"create skill"}' | .claude/hooks/skill-activation-prompt.sh`

### Step 2.3: Create skill-rules.json
**Create**: `/.claude/skills/skill-rules.json`
```json
{
  "version": "1.0",
  "skills": {
    "skill-developer": {
      "type": "domain",
      "enforcement": "suggest",
      "priority": "high",
      "promptTriggers": {
        "keywords": ["skill system", "create skill", "add skill", "skill triggers", "hook system"],
        "intentPatterns": ["(create|add|modify).*?skill", "skill.*?(work|trigger|activate)"]
      }
    },
    "blockchain-dev-guidelines": {
      "type": "domain",
      "enforcement": "suggest",
      "priority": "high",
      "promptTriggers": {
        "keywords": ["consensus", "hotstuff", "orderbook", "block", "vote", "certificate", "BLS", "AppHook", "BlockStore", "mempool", "matching engine", "liquidation", "funding"],
        "intentPatterns": ["(create|add|modify).*?(consensus|block|vote|order)", "(how|what).*?(consensus|orderbook|matching)", "rust.*?(pattern|convention)"]
      },
      "fileTriggers": {
        "pathPatterns": ["src/**/*.rs"]
      }
    },
    "frontend-dev-guidelines": {
      "type": "domain",
      "enforcement": "suggest",
      "priority": "high",
      "promptTriggers": {
        "keywords": ["component", "zustand", "tailwind", "websocket", "wallet", "next.js", "trading UI", "orderbook UI", "toast"],
        "intentPatterns": ["(create|add|modify).*?(component|page|hook)", "(how|what).*?(zustand|tailwind|wallet)", "frontend.*?(pattern|convention)"]
      },
      "fileTriggers": {
        "pathPatterns": ["web/**/*.tsx", "web/**/*.ts"]
      }
    }
  }
}
```

---

## Phase 3: skill-developer Meta-Skill

### Step 3.1: Copy skill-developer files
**Create directory**: `/.claude/skills/skill-developer/`

**Copy from** `claude-code-practice/.claude/skills/skill-developer/`:
- `SKILL.md` (main file, <500 lines)
- `SKILL_RULES_REFERENCE.md`
- `TRIGGER_TYPES.md`
- `HOOK_MECHANISMS.md`
- `TROUBLESHOOTING.md`
- `PATTERNS_LIBRARY.md`
- `ADVANCED.md`

**Adapt SKILL.md**:
- Remove PreToolUse/blocking references (not implementing)
- Update paths to hyperlicked structure

**Verify**: `wc -l .claude/skills/skill-developer/SKILL.md` (expect <500)

### Step 3.2: Create skills README
**Create**: `/.claude/skills/README.md`

---

## Phase 4: blockchain-dev-guidelines Skill

### Step 4.1: Create skill directory
**Create directory**: `/.claude/skills/blockchain-dev-guidelines/`

### Step 4.2: Create SKILL.md (main file, <500 lines)
**Create**: `/.claude/skills/blockchain-dev-guidelines/SKILL.md`

Content covers (high-level overview):
- Project structure (src/ modules)
- Core types (i64 for prices/sizes, Hash, NodeId)
- Key traits (AppHook, BlockStore, Network)
- Consensus patterns (HotStuff-2, 2-chain commit)
- Error handling (thiserror enums)
- Configuration (environment variables, Mode enum)
- Links to reference files for details

### Step 4.3: Create reference files (progressive disclosure)
**Create**: `/.claude/skills/blockchain-dev-guidelines/resources/`

| File | Content | ~Lines |
|------|---------|--------|
| `CONSENSUS.md` | HotStuff-2 engine, tick pattern, leader/follower, Block/Vote/Certificate | 400 |
| `ORDERBOOK.md` | Heap-based matching, 3-bucket mempool, Fill/Order types, FIFO | 350 |
| `TYPES.md` | Integer math, type aliases, Position, Account, MarketConfig | 300 |
| `CRYPTO.md` | BLS signatures, EIP-712, agent keys, signature verification | 250 |
| `TESTING.md` | Integration tests, single-node tests, determinism | 200 |
| `PATTERNS.md` | Module organization, error enums, serialization, 500 LOC rule | 250 |

**Verify**: `wc -l .claude/skills/blockchain-dev-guidelines/SKILL.md` (expect <500)

---

## Phase 5: frontend-dev-guidelines Skill

### Step 5.1: Create skill directory
**Create directory**: `/.claude/skills/frontend-dev-guidelines/`

### Step 5.2: Create SKILL.md (main file, <500 lines)
**Create**: `/.claude/skills/frontend-dev-guidelines/SKILL.md`

Content covers (high-level overview):
- Project structure (web/ with app router)
- State management (Zustand stores pattern)
- Component patterns (client directive, structure)
- Styling (Tailwind, cn utility, theme colors)
- API integration (REST client, unit conversion)
- Wallet integration (EIP-712, agent keys)
- Links to reference files for details

### Step 5.3: Create reference files (progressive disclosure)
**Create**: `/.claude/skills/frontend-dev-guidelines/resources/`

| File | Content | ~Lines |
|------|---------|--------|
| `COMPONENTS.md` | Component structure, client directive, patterns | 350 |
| `STATE.md` | Zustand stores, WalletStore, TradingStore patterns | 300 |
| `STYLING.md` | Tailwind conventions, theme colors, cn utility | 250 |
| `API.md` | REST client, unit conversion, WebSocket integration | 350 |
| `WALLET.md` | useWallet hook, agent keys, EIP-712 signing | 400 |
| `HOOKS.md` | useCallback, useEffect, useRef patterns | 250 |

**Verify**: `wc -l .claude/skills/frontend-dev-guidelines/SKILL.md` (expect <500)

---

## Phase 6: Dev Docs System

### Step 6.1: Create docs/dev structure
**Create**: `/docs/dev/README.md`
```markdown
# Dev Docs

Context persistence files for AI-assisted development.

## Structure
- `{task-name}-plan.md` - Strategic plan
- `{task-name}-context.md` - Key files, decisions, state
- `{task-name}-tasks.md` - Actionable checklist

## Usage
1. Read CLAUDE.md for project overview
2. Check docs/blockchain/ROADMAP.md for priorities
3. Check docs/dev/ for active task context
```

### Step 6.2: Update doc-sync command
**Modify**: `/.claude/commands/doc-sync.md`
- Add reference to docs/dev/ for task context

---

## Phase 7: Integration & Test

### Step 7.1: Make scripts executable
```bash
chmod +x .claude/hooks/*.sh
```

### Step 7.2: Install dependencies
```bash
cd .claude/hooks && npm install
```

### Step 7.3: End-to-end test
```bash
echo '{"session_id":"test","prompt":"how do I create a skill?"}' | \
  npx tsx .claude/hooks/skill-activation-prompt.ts
```

**Expected output**: Skill suggestion for skill-developer

---

## Files Summary

### Create New
| File | Purpose |
|------|---------|
| `/.claude/settings.json` | Hook registration |
| `/.claude/hooks/package.json` | Node dependencies |
| `/.claude/hooks/tsconfig.json` | TypeScript config |
| `/.claude/hooks/skill-activation-prompt.sh` | Bash wrapper |
| `/.claude/hooks/skill-activation-prompt.ts` | Hook logic |
| `/.claude/skills/skill-rules.json` | Skill triggers (3 skills) |
| `/.claude/skills/README.md` | Skills overview |
| `/.claude/skills/skill-developer/SKILL.md` | Meta-skill main |
| `/.claude/skills/skill-developer/resources/*` | 6 reference files |
| `/.claude/skills/blockchain-dev-guidelines/SKILL.md` | Rust/consensus main |
| `/.claude/skills/blockchain-dev-guidelines/resources/*` | 6 reference files |
| `/.claude/skills/frontend-dev-guidelines/SKILL.md` | Next.js/Tailwind main |
| `/.claude/skills/frontend-dev-guidelines/resources/*` | 6 reference files |
| `/docs/dev/README.md` | Dev docs overview |
| `/docs/features/001-claude-code-setup/PLAN.md` | Copy of this plan |

### Modify Existing
| File | Change |
|------|--------|
| `/.claude/hooks/post-work-verify.sh` | ./rust → ./src |
| `/.claude/commands/doc-sync.md` | Add docs/dev reference |

### Total Files
- **New files**: ~28 files
- **Modified files**: 2 files

---

## Verification Checklist

- [x] `jq . .claude/settings.json` - Valid JSON
- [x] `jq . .claude/skills/skill-rules.json` - Valid JSON with 3 skills
- [x] `cargo build` runs from project root (post-work-verify.sh fixed)
- [x] Skill activation hook returns suggestions for "create skill" prompt
- [x] Skill activation hook returns suggestions for "add consensus" prompt
- [x] Skill activation hook returns suggestions for "add component" prompt
- [x] skill-developer SKILL.md is under 500 lines
- [x] blockchain-dev-guidelines SKILL.md is under 500 lines
- [x] frontend-dev-guidelines SKILL.md is under 500 lines
- [x] All reference files are under 500 lines
- [x] docs/dev/README.md exists
- [x] docs/features/001-claude-code-setup/PLAN.md exists

---

## Context Optimization

**During implementation:**
1. Batch file creations by phase
2. Copy skill-developer files wholesale, then adapt
3. Run verification after each phase before proceeding
4. Create all 3 SKILL.md files first (main files), then batch all resources

**After context reset, read in order:**
1. `CLAUDE.md` - Project overview
2. `docs/blockchain/ROADMAP.md` - Current priorities
3. `docs/dev/README.md` - Active task context

---

## Key Reference Files (for implementation)

Source files to reference from claude-code-practice:
- `.claude/hooks/skill-activation-prompt.ts` - Hook logic
- `.claude/skills/skill-developer/SKILL.md` - Skill structure template
- `.claude/skills/skill-rules.json` - Rules schema

Exploration data to use:
- Rust patterns from blockchain exploration (agent a0c33ad)
- Next.js patterns from frontend exploration (agent a1f1912)
