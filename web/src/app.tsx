import { useComputed, useSignal } from '@preact/signals'
import { useEffect, useLayoutEffect, useRef } from 'preact/hooks'
import { agents, blocks, connected, selected } from './store'
import type { Agent, Block, EventKind } from './types'

const FILTERS: { id: EventKind | 'all'; label: string }[] = [
  { id: 'all', label: 'all' },
  { id: 'thought', label: 'thinking' },
  { id: 'message', label: 'messages' },
  { id: 'tool_call', label: 'tools' },
  { id: 'error', label: 'errors' }
]

function clock(ms: number): string {
  return new Date(ms).toTimeString().slice(0, 8)
}

function ago(ms: number | null): string {
  if (!ms) return '—'
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000))
  if (s < 60) return `${s}s`
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`
}

function AgentRow({ agent, tick }: { agent: Agent; tick: number }) {
  void tick // re-render for the live timer
  const busy = agent.connected && agent.turn_started_ms !== null
  const isSel = selected.value === agent.label

  return (
    <button
      class={`agent ${isSel ? 'sel' : ''}`}
      onClick={() => (selected.value = isSel ? null : agent.label)}
    >
      <span class={`dot ${busy ? 'busy' : agent.connected ? 'on' : 'off'}`} />
      <span class="agent-body">
        <span class="agent-name">{agent.label}</span>
        <span class="agent-meta">
          {busy ? (
            <span class="live">running {ago(agent.turn_started_ms)}</span>
          ) : (
            <>idle {ago(agent.last_activity_ms)}</>
          )}
        </span>
      </span>
      <span class="agent-counts">
        <span title="turns">{agent.turns}t</span>
        <span title="tool calls">{agent.tool_calls}⚒</span>
      </span>
    </button>
  )
}

function ToolBlock({ block }: { block: Block }) {
  const status = block.status ?? 'pending'
  return (
    <div class={`row tool ${status}`}>
      <span class="ts">{clock(block.ts_ms)}</span>
      <span class="gutter">⚒</span>
      <div class="content">
        <span class="tool-title">{block.text}</span>
        {block.toolKind && <span class="chip">{block.toolKind}</span>}
        <span class={`chip status ${status}`}>{status}</span>
      </div>
    </div>
  )
}

function Row({ block, showLabel }: { block: Block; showLabel: boolean }) {
  if (block.kind === 'tool_call') return <ToolBlock block={block} />

  const gutter: Record<string, string> = {
    thought: '',
    message: '',
    turn_started: '▸',
    turn_ended: '■',
    tool_update: '·',
    session_created: '＋',
    plan: '☰',
    error: '!'
  }

  return (
    <div class={`row ${block.kind}`}>
      <span class="ts">{clock(block.ts_ms)}</span>
      <span class="gutter">{gutter[block.kind] ?? ''}</span>
      <div class="content">
        {showLabel && <span class="who">{block.label}</span>}
        {block.kind === 'turn_started' && <span class="kindtag">prompt</span>}
        {block.kind === 'turn_ended' && <span class="kindtag">turn end</span>}
        <span class="text">{block.text}</span>
      </div>
    </div>
  )
}

export function App() {
  const filter = useSignal<EventKind | 'all'>('all')
  const query = useSignal('')
  const tick = useSignal(0)
  const atBottom = useSignal(true)
  const feedRef = useRef<HTMLDivElement>(null)
  const stick = useRef(true)

  useEffect(() => {
    const id = setInterval(() => tick.value++, 1000)
    return () => clearInterval(id)
  }, [])

  const shown = useComputed(() => {
    const sel = selected.value
    const f = filter.value
    const q = query.value.trim().toLowerCase()

    return blocks.value.filter(b => {
      if (sel && b.label !== sel) return false
      if (f !== 'all') {
        if (f === 'tool_call' && b.kind !== 'tool_call' && b.kind !== 'tool_update') return false
        if (f !== 'tool_call' && b.kind !== f) return false
      }
      if (q && !b.text.toLowerCase().includes(q) && !b.label.toLowerCase().includes(q)) return false
      return true
    })
  })

  // Keep the newest row in view unless the user has scrolled up to read.
  // Keyed on the array identity, not its length: streamed chunks merge into the
  // last block, so the row count stays flat while the content keeps growing.
  useLayoutEffect(() => {
    const el = feedRef.current
    if (el && stick.current) el.scrollTop = el.scrollHeight
  }, [shown.value])

  const onScroll = () => {
    const el = feedRef.current
    if (!el) return
    stick.current = el.scrollTop + el.clientHeight >= el.scrollHeight - 60
    atBottom.value = stick.current
  }

  const jumpToLive = () => {
    const el = feedRef.current
    if (!el) return
    stick.current = true
    atBottom.value = true
    el.scrollTop = el.scrollHeight
  }

  const running = useComputed(() => agents.value.filter(a => a.turn_started_ms !== null).length)

  return (
    <div class="app">
      <aside>
        <header>
          <div class="brand">
            acp<span>·</span>tap
          </div>
          <div class={`link ${connected.value ? 'up' : 'down'}`}>
            {connected.value ? 'live' : 'reconnecting'}
          </div>
        </header>

        <div class="agents">
          {agents.value.length === 0 && <div class="empty small">no taps connected</div>}
          {agents.value.map(a => (
            <AgentRow key={a.label} agent={a} tick={tick.value} />
          ))}
        </div>

        <footer>
          {agents.value.length} agents · {running.value} running
        </footer>
      </aside>

      <main class="main">
        <header>
          <h1>{selected.value ?? 'all agents'}</h1>
          {selected.value && (
            <button class="clear" onClick={() => (selected.value = null)}>
              clear
            </button>
          )}
          <div class="filters">
            {FILTERS.map(f => (
              <button
                key={f.id}
                class={`chip filter ${filter.value === f.id ? 'on' : ''}`}
                onClick={() => (filter.value = f.id)}
              >
                {f.label}
              </button>
            ))}
          </div>
          <input
            class="search"
            placeholder="search…"
            value={query.value}
            onInput={e => (query.value = (e.target as HTMLInputElement).value)}
          />
          <span class="count">{shown.value.length}</span>
        </header>

        <div class="feed" ref={feedRef} onScroll={onScroll}>
          {shown.value.length === 0 ? (
            <div class="empty">
              <p>Nothing yet.</p>
              <code>acp-tap --label my-agent -- pi-acp</code>
            </div>
          ) : (
            shown.value.map(b => <Row key={b.id} block={b} showLabel={!selected.value} />)
          )}
        </div>

        {!atBottom.value && (
          <button class="jump" onClick={jumpToLive}>
            jump to live ↓
          </button>
        )}
      </main>
    </div>
  )
}
