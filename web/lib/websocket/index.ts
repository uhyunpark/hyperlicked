/**
 * WebSocket Module - Real-time data integration
 *
 * This module is split into focused parts:
 * - types.ts: TypeScript interfaces for all WebSocket messages
 * - handlers.ts: Message handlers for each event type
 * - useWebSocketConnection.ts: Connection lifecycle and reconnect logic
 *
 * @see lib/websocket/types.ts
 * @see lib/websocket/handlers.ts
 * @see lib/websocket/useWebSocketConnection.ts
 */

export { useWebSocketConnection } from './useWebSocketConnection'
export * from './types'
