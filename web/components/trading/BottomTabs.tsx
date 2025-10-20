'use client'

import { useState } from 'react'
import { RecentTrades } from './RecentTrades'
import { OpenOrders } from './OpenOrders'
import { Positions } from './Positions'

type Tab = 'trades' | 'orders' | 'positions'

export function BottomTabs() {
  const [activeTab, setActiveTab] = useState<Tab>('trades')

  const tabs: { id: Tab; label: string }[] = [
    { id: 'trades', label: 'Recent Trades' },
    { id: 'orders', label: 'Open Orders' },
    { id: 'positions', label: 'Positions' }
  ]

  return (
    <div className="flex h-full flex-col bg-bg-secondary">
      {/* Tabs */}
      <div className="flex border-b border-border">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            onClick={() => setActiveTab(tab.id)}
            className={`px-6 py-2 text-sm font-medium transition-colors ${
              activeTab === tab.id
                ? 'border-b-2 border-accent text-text-primary'
                : 'text-text-muted hover:text-text-secondary'
            }`}
          >
            {tab.label}
          </button>
        ))}
      </div>

      {/* Tab content */}
      <div className="flex-1 overflow-hidden">
        {activeTab === 'trades' && <RecentTrades />}
        {activeTab === 'orders' && <OpenOrders />}
        {activeTab === 'positions' && <Positions />}
      </div>
    </div>
  )
}
