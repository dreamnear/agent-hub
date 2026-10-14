// Ported from Beautiful UI (https://beautifului.dev) © Shane Levine / Turbo, MIT
// 桥接改造（视觉类名/结构原样）：
// 1. 官网内部依赖 @/components/atoms/Button 内联为等价类组合
//    （bg-accent text-white rounded-full px-3 h-[27px] text-[12px] font-medium），
//    避免引入官网 atoms 体系
// 2. primitive-card-bar/table-cell/card-footer 类见 src/bui/bui-motion.css

import { useEffect, useState } from 'react';

/* ─────────────────────────────────────────────────────────
 * DIFF TABLE
 * The proposed edit plays once and rests on the completed
 * diff. Each changed row is the control: click it to include
 * or exclude that specific addition/removal before applying.
 * ───────────────────────────────────────────────────────── */

function useStage(steps: number[]) {
  const [stage, setStage] = useState(0);
  useEffect(() => {
    if (stage >= steps.length) return;
    const t = setTimeout(() => setStage((s) => s + 1), steps[stage]);
    return () => clearTimeout(t);
  }, [stage, steps]);
  return stage;
}

const STAGE_DELAYS = [180, 260];

export type DiffRow = {
  key: string;
  id: string;
  dept: string;
  email: string;
  removed: boolean;
};

const ROWS: DiffRow[] = [
  { key: 'rocky', id: 'Rocky Road', dept: 'Classic', email: 'aurora-scoops', removed: true },
  { key: 'bubblegum', id: 'Bubblegum', dept: 'Retro', email: 'kumo-creamery', removed: true },
  { key: 'mint', id: 'Mint Chip', dept: 'Classic', email: 'maple-orbit', removed: false },
];

const DOT: Record<string, string> = {
  Classic: 'bg-accent',
  Retro: 'bg-ink-3',
  Seasonal: 'bg-orange',
};

function IncludedMark({ included, tone }: { included: boolean; tone: 'red' | 'green' }) {
  return (
    <span
      aria-hidden
      className={`flex size-4.5 shrink-0 items-center justify-center rounded-[5px] transition-[background-color,color,transform] duration-150 ${
        included
          ? tone === 'red' ? 'bg-red text-white' : 'bg-green text-white'
          : 'bg-inset text-ink-3 shadow-hairline'
      }`}
      style={{ transform: included ? 'scale(1)' : 'scale(0.92)' }}
    >
      {included ? (
        <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round"><path d="M20 6L9 17l-5-5" /></svg>
      ) : null}
    </span>
  );
}

export default function DiffTable({
  rows = ROWS,
}: {
  rows?: DiffRow[];
  variant?: string;
} = {}) {
  const stage = useStage(STAGE_DELAYS);
  // 0 plain · 1 removals · 2 completed diff
  const tinted = stage >= 1;
  const settled = stage >= 2;
  const [accepted, setAccepted] = useState(false);
  const [edits, setEdits] = useState<Record<string, boolean>>({ rocky: true, bubblegum: true, pistachio: true });

  const removals = ['rocky', 'bubblegum'].filter((key) => edits[key]).length;
  const additions = edits.pistachio ? 1 : 0;
  const showAdded = settled;

  const toggleEdit = (key: string) => setEdits((current) => ({ ...current, [key]: !current[key] }));

  return (
    <div className="w-full max-w-95">
      <div className="relative overflow-hidden rounded-card bg-surface shadow-card">
        <div className="primitive-card-bar flex items-center justify-between border-b border-line">
          <span className="text-[12.5px] font-medium text-ink">Proposed menu cleanup</span>
          {settled && !accepted && <span className="text-[11px] text-ink-3">Click changed rows to toggle</span>}
        </div>

        <table className="w-full table-fixed border-collapse text-left">
          <colgroup>
            <col className="w-[34%]" />
            <col className="w-[30%]" />
            <col className="w-[36%]" />
          </colgroup>
          <thead>
            <tr className="border-b border-line">
              {['Flavor', 'Category', 'Supplier'].map((h) => (
                <th key={h} className="primitive-table-cell text-[12px] font-medium text-ink-3">
                  {h}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => {
              const out = row.removed && tinted && edits[row.key];
              const interactive = row.removed && settled && !accepted;
              return (
                <tr
                  key={row.key}
                  tabIndex={interactive ? 0 : undefined}
                  aria-selected={row.removed ? edits[row.key] : undefined}
                  onClick={interactive ? () => toggleEdit(row.key) : undefined}
                  onKeyDown={interactive ? (event) => {
                    if (event.key === 'Enter' || event.key === ' ') {
                      event.preventDefault();
                      toggleEdit(row.key);
                    }
                  } : undefined}
                  className={`border-b border-line transition-[background-color,filter,opacity] duration-150 last:border-0 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-accent ${
                    interactive ? 'cursor-pointer hover:brightness-[0.985]' : ''
                  }`}
                  style={{ background: out ? 'var(--red-tint)' : undefined }}
                >
                  <td
                    className="primitive-table-cell text-[13px] font-medium tabular-nums transition-colors duration-200"
                    style={{ color: out ? 'var(--red)' : 'var(--ink)' }}
                  >
                    {row.id}
                  </td>
                  <td className="primitive-table-cell">
                    <span
                      className="inline-flex h-5.5 items-center gap-1.5 rounded-full bg-inset px-2 text-[11.5px] font-medium shadow-hairline transition-opacity duration-200"
                      style={{ opacity: out ? 0.55 : 1 }}
                    >
                      <span className={`size-1.5 rounded-full ${DOT[row.dept]}`} />
                      <span className="text-ink-2">{row.dept}</span>
                    </span>
                  </td>
                  <td
                    className="primitive-table-cell text-[12.5px] whitespace-nowrap transition-colors duration-200"
                    style={{
                      color: out ? 'var(--red)' : 'var(--ink-2)',
                      textDecorationLine: out ? 'line-through' : 'none',
                      textDecorationColor: 'color-mix(in srgb, var(--red) 50%, transparent)',
                    }}
                  >
                    <span className="flex items-center justify-between gap-2">
                      <span className="min-w-0 truncate">{row.email}</span>
                      {row.removed && settled && <IncludedMark included={edits[row.key]} tone="red" />}
                    </span>
                  </td>
                </tr>
              );
            })}
            {/* added row */}
            <tr>
              <td colSpan={3} className="p-0">
                <div
                  className="grid transition-[grid-template-rows,opacity] duration-200"
                  style={{
                    gridTemplateRows: showAdded ? '1fr' : '0fr',
                    opacity: showAdded ? 1 : 0,
                    transitionTimingFunction: 'cubic-bezier(0.23, 1, 0.32, 1)',
                  }}
                >
                  <div className="overflow-hidden">
                    <div
                      role="checkbox"
                      tabIndex={accepted ? -1 : 0}
                      aria-checked={edits.pistachio}
                      aria-label="Include adding Pistachio"
                      onClick={accepted ? undefined : () => toggleEdit('pistachio')}
                      onKeyDown={accepted ? undefined : (event) => {
                        if (event.key === 'Enter' || event.key === ' ') {
                          event.preventDefault();
                          toggleEdit('pistachio');
                        }
                      }}
                      className={`grid grid-cols-[34%_30%_36%] items-center border-t border-line transition-[background-color,filter,opacity] duration-150 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-accent ${
                        accepted ? '' : 'cursor-pointer hover:brightness-[0.985]'
                      }`}
                      style={{ background: edits.pistachio ? 'var(--green-tint)' : undefined }}
                    >
                      <span className="primitive-table-cell text-[13px] font-medium tabular-nums transition-colors duration-200" style={{ color: edits.pistachio ? 'var(--green)' : 'var(--ink-3)' }}>
                        Pistachio
                      </span>
                      <span className="primitive-table-cell">
                        <span className="inline-flex h-5.5 items-center gap-1.5 rounded-full bg-surface px-2 text-[11.5px] font-medium shadow-hairline">
                          <span className="size-1.5 rounded-full bg-green" />
                          <span className="text-ink-2">Seasonal</span>
                        </span>
                      </span>
                      <span className="primitive-table-cell text-[13px] transition-colors duration-200" style={{ color: edits.pistachio ? 'var(--green)' : 'var(--ink-3)' }}>
                        <span className="flex items-center justify-between gap-2">
                          <span className="min-w-0 truncate">maple-orbit</span>
                          <IncludedMark included={edits.pistachio} tone="green" />
                        </span>
                      </span>
                    </div>
                  </div>
                </div>
              </td>
            </tr>
          </tbody>
        </table>

        {/* footer — the summary follows the row-level selection */}
        {settled && (
          <div
            className="primitive-card-footer flex min-h-11 items-center justify-between border-t border-line"
            style={{ animation: 'fade-up 180ms cubic-bezier(0.23,1,0.32,1) both' }}
          >
            {accepted ? (
              <span
                className="inline-flex items-center gap-1.5 rounded-full bg-green-tint py-1 pr-2.5 pl-1 text-[12.5px] font-medium text-green"
                style={{ animation: 'pop-in 180ms cubic-bezier(0.23,1,0.32,1) both' }}
              >
                <span className="flex size-4.5 items-center justify-center rounded-full bg-green text-white">
                  <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round"><path d="M20 6L9 17l-5-5" /></svg>
                </span>
                {removals + additions} {removals + additions === 1 ? 'edit' : 'edits'} applied
              </span>
            ) : (
              <>
                <span className="text-[11.5px] tabular-nums text-ink-3">
                  {removals} {removals === 1 ? 'removal' : 'removals'} · {additions} {additions === 1 ? 'addition' : 'additions'}
                </span>
                {/* 官网 atoms/Button accent 内联等价（见文件头桥接注释） */}
                <button
                  type="button"
                  disabled={removals + additions === 0}
                  onClick={() => setAccepted(true)}
                  className="inline-flex h-[27px] items-center justify-center rounded-full bg-accent px-3 text-[12px] font-medium text-white select-none transition-[transform,background-color,opacity] duration-150 ease-out active:scale-[0.96] disabled:pointer-events-none disabled:opacity-50"
                >
                  Apply {removals + additions} {removals + additions === 1 ? 'change' : 'changes'}
                </button>
              </>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
