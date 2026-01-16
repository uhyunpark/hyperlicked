# Styling Patterns

Reference for Tailwind CSS conventions and theme colors.

## Table of Contents

- [Theme Colors](#theme-colors)
- [cn Utility](#cn-utility)
- [Component Styling](#component-styling)
- [Responsive Design](#responsive-design)

---

## Theme Colors

### Background Colors

```css
/* globals.css */
:root {
  --primary: #0d0d14;     /* Darkest - main background */
  --secondary: #1a1a24;   /* Cards, panels */
  --tertiary: #252532;    /* Hover states */
  --accent: #7c3aed;      /* Purple accent */
}
```

```typescript
// Usage
className="bg-primary"     // Main background
className="bg-secondary"   // Card background
className="bg-tertiary"    // Hover/active state
className="bg-accent"      // Accent elements
```

### Text Colors

```typescript
className="text-primary"   // Main text (white/light)
className="text-secondary" // Secondary text (gray)
className="text-muted"     // Muted text (darker gray)
className="text-accent"    // Accent text (purple)
```

### Semantic Colors

```typescript
// Buy/Sell
className="text-green-buy"  // Green for buys/long
className="text-red-sell"   // Red for sells/short

// Background variants
className="bg-green-buy/10"  // Green with 10% opacity
className="bg-red-sell/10"   // Red with 10% opacity
```

### Border Colors

```typescript
className="border-border"   // Standard border (#2d2d3d)
className="border-accent"   // Accent border
```

---

## cn Utility

### Definition

```typescript
// lib/utils.ts
import { type ClassValue, clsx } from 'clsx'
import { twMerge } from 'tailwind-merge'

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}
```

### Why Use cn?

Combines `clsx` (conditional classes) with `tailwind-merge` (resolves conflicts):

```typescript
// Without cn - conflicting classes
className="p-4 p-2"  // Both applied, unpredictable

// With cn - last wins
cn('p-4', 'p-2')     // → "p-2"

// With cn - conditional
cn('flex', isActive && 'bg-accent')  // → "flex bg-accent" or "flex"
```

### Usage Patterns

```typescript
// Basic
className={cn('flex items-center')}

// Conditional
className={cn(
  'flex items-center',
  isActive && 'bg-accent',
  disabled && 'opacity-50'
)}

// Dynamic
className={cn(
  'px-4 py-2',
  variant === 'primary' && 'bg-accent text-white',
  variant === 'secondary' && 'bg-secondary text-primary'
)}

// Override defaults
className={cn(
  'text-sm',  // default
  className   // allow override from props
)}
```

---

## Component Styling

### Container Patterns

```typescript
// Full height flex column
className="flex h-full flex-col"

// Scrollable content
className="flex-1 overflow-y-auto"

// Fixed header
className="border-b border-border px-4 py-2"

// Grid layout
className="grid grid-cols-2 gap-4"
```

### Card/Panel

```typescript
className="bg-secondary rounded-lg border border-border p-4"
```

### Table Styling

```typescript
// Header
<th className="text-left text-xs text-muted px-4 py-2">

// Row
<tr className="hover:bg-tertiary transition-colors">

// Cell
<td className="px-4 py-2 font-mono text-sm">
```

### Button Patterns

```typescript
// Primary button
className={cn(
  'px-4 py-2 rounded-lg font-medium',
  'bg-accent text-white',
  'hover:bg-accent/90',
  'disabled:opacity-50 disabled:cursor-not-allowed'
)}

// Secondary button
className={cn(
  'px-4 py-2 rounded-lg font-medium',
  'bg-secondary text-primary',
  'hover:bg-tertiary',
  'border border-border'
)}

// Buy/Sell buttons
className={cn(
  'flex-1 py-3 rounded-lg font-semibold',
  side === 'bid'
    ? 'bg-green-buy text-white'
    : 'bg-red-sell text-white'
)}
```

### Input Styling

```typescript
className={cn(
  'w-full px-3 py-2 rounded-lg',
  'bg-primary border border-border',
  'text-primary placeholder-muted',
  'focus:outline-none focus:border-accent'
)}
```

### Toggle/Tab

```typescript
className={cn(
  'px-4 py-2 rounded-lg text-sm',
  isActive
    ? 'bg-accent text-white'
    : 'text-muted hover:text-primary'
)}
```

---

## Responsive Design

### Breakpoints

```typescript
// Mobile first
className="w-full md:w-1/2 lg:w-1/3"

// Hide/show
className="hidden md:block"
className="block md:hidden"

// Responsive text
className="text-sm md:text-base lg:text-lg"
```

### Common Patterns

```typescript
// Responsive grid
className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4"

// Responsive padding
className="px-4 md:px-6 lg:px-8"

// Responsive flex direction
className="flex flex-col md:flex-row"
```

---

## Special Patterns

### Price Display

```typescript
// Monospace for numbers
className="font-mono"

// Color based on side
className={cn(
  'font-mono',
  side === 'bid' ? 'text-green-buy' : 'text-red-sell'
)}
```

### Depth Visualization

```typescript
// Background bar for orderbook depth
<div
  className={cn(
    'absolute inset-0',
    side === 'bid' ? 'bg-green-buy/10' : 'bg-red-sell/10'
  )}
  style={{ width: `${depthPercent}%` }}
/>
```

### Loading State

```typescript
className="animate-pulse bg-tertiary rounded"
```

### Connection Indicator

```typescript
// Green dot for connected
<div className="h-2 w-2 rounded-full bg-green-500" />

// Red dot for disconnected
<div className="h-2 w-2 rounded-full bg-red-500" />
```

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [COMPONENTS.md](COMPONENTS.md) - Component patterns
- [HOOKS.md](HOOKS.md) - Hook patterns
