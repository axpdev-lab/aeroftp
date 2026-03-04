import { useState, useEffect, useRef, useCallback } from 'react';
import { X, Sparkles } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useLicense } from '../hooks/useLicense';

const NAG_INTERVAL_MS = 30 * 60 * 1000; // 30 minutes

/** Non-intrusive banner for free users, appears every 30 minutes. */
export default function NagBanner() {
  const t = useTranslation();
  const { isPro, loading } = useLicense();
  const [visible, setVisible] = useState(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const showBanner = useCallback(() => {
    setVisible(true);
  }, []);

  const dismiss = useCallback(() => {
    setVisible(false);
    // Schedule next appearance
    if (timerRef.current) clearTimeout(timerRef.current);
    timerRef.current = setTimeout(showBanner, NAG_INTERVAL_MS);
  }, [showBanner]);

  useEffect(() => {
    if (loading || isPro) {
      setVisible(false);
      if (timerRef.current) { clearTimeout(timerRef.current); timerRef.current = null; }
      return;
    }

    // Show after 5 minutes on first use, then every 30 minutes
    timerRef.current = setTimeout(showBanner, 5 * 60 * 1000);

    return () => {
      if (timerRef.current) { clearTimeout(timerRef.current); timerRef.current = null; }
    };
  }, [isPro, loading, showBanner]);

  if (!visible || isPro || loading) return null;

  return (
    <div className="fixed bottom-4 right-4 z-40 max-w-sm animate-scale-in">
      <div className="bg-gradient-to-r from-blue-600 to-purple-600 text-white rounded-xl shadow-lg px-4 py-3 flex items-start gap-3">
        <Sparkles size={18} className="shrink-0 mt-0.5" />
        <div className="flex-1 min-w-0">
          <div className="text-sm font-medium">
            {t('license.nagTitle')}
          </div>
          <div className="text-xs opacity-80 mt-0.5">
            {t('license.nagDescription')}
          </div>
        </div>
        <button
          onClick={dismiss}
          className="shrink-0 p-0.5 rounded hover:bg-white/20 transition-colors"
          title={t('common.close')}
        >
          <X size={14} />
        </button>
      </div>
    </div>
  );
}
