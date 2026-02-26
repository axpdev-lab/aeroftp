import { Lock } from 'lucide-react';

interface FeatureBadgeProps {
  value: string | null | undefined;
  emptyLabel?: string;
  locked?: boolean;
}

const BADGE_COLORS: Record<string, string> = {
  private: 'bg-red-100 text-red-700 dark:bg-red-900/40 dark:text-red-300',
  public: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300',
  enabled: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300',
  disabled: 'bg-gray-200 text-gray-700 dark:bg-gray-700 dark:text-gray-300',
  yes: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300',
  no: 'bg-gray-200 text-gray-700 dark:bg-gray-700 dark:text-gray-300',
  on: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300',
  off: 'bg-gray-200 text-gray-700 dark:bg-gray-700 dark:text-gray-300',
};

export function FeatureBadge({ value, emptyLabel = '-', locked = false }: FeatureBadgeProps) {
  const text = (value || '').trim();
  if (!text) {
    if (!locked) {
      return <span className="text-xs text-gray-400">{emptyLabel}</span>;
    }
    return (
      <span
        className="inline-flex items-center rounded-full px-1.5 py-0.5 text-[10px] bg-amber-100 text-amber-700 dark:bg-amber-900/40 dark:text-amber-300"
        title="Password protected"
      >
        <Lock size={10} />
      </span>
    );
  }

  const badgeClass = BADGE_COLORS[text.toLowerCase()] || 'bg-gray-100 text-gray-700 dark:bg-gray-700 dark:text-gray-200';

  return (
    <span
      className={`inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium whitespace-nowrap ${badgeClass}`}
      title={text}
    >
      {locked && <Lock size={10} className="mr-1" />}
      {text}
    </span>
  );
}
