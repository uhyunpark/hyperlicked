'use client'

import { useState } from 'react'
import { OpenOrders } from './OpenOrders'
import { Positions } from './Positions'
import { Balances } from './Balances'
import { TradeHistory } from './TradeHistory'
import { FundingHistory } from './FundingHistory'
import { OrderHistory } from './OrderHistory'

type Tab = 'balances' | 'positions' | 'orders' | 'twap' | 'trade-history' | 'funding-history' | 'order-history'

interface TabConfig {
  id: Tab
  label: string
  disabled?: boolean
}

export function BottomTabs() {
  const [activeTab, setActiveTab] = useState<Tab>('positions')

  const tabs: TabConfig[] = [
    { id: 'balances', label: 'Balances' },
    { id: 'positions', label: 'Positions' },
    { id: 'orders', label: 'Open Orders' },
    { id: 'twap', label: 'TWAP', disabled: true },
    { id: 'trade-history', label: 'Trade History' },
    { id: 'funding-history', label: 'Funding History' },
    { id: 'order-history', label: 'Order History' }
  ]

  return (
    <div className="flex h-full flex-col bg-bg-secondary">
      {/* Tabs */}
      <div className="flex border-b border-border">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            onClick={() => !tab.disabled && setActiveTab(tab.id)}
            disabled={tab.disabled}
            className={`px-4 py-2 text-xs font-medium transition-colors ${
              tab.disabled
                ? 'cursor-not-allowed text-text-muted/50'
                : activeTab === tab.id
                  ? 'border-b-2 border-accent text-text-primary'
                  : 'text-text-muted hover:text-text-secondary'
            }`}
            title={tab.disabled ? 'Coming Soon' : undefined}
          >
            {tab.label}
          </button>
        ))}
      </div>

      {/* Tab content */}
      <div className="flex-1 overflow-hidden">
        {activeTab === 'balances' && <Balances />}
        {activeTab === 'positions' && <Positions />}
        {activeTab === 'orders' && <OpenOrders />}
        {activeTab === 'twap' && (
          <div className="flex h-full items-center justify-center text-text-muted">
            TWAP orders coming soon
          </div>
        )}
        {activeTab === 'trade-history' && <TradeHistory />}
        {activeTab === 'funding-history' && <FundingHistory />}
        {activeTab === 'order-history' && <OrderHistory />}
      </div>
    </div>
  )
}
