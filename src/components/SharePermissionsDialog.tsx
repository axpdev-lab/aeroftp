import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { X, UserPlus, Trash2, RefreshCw, Users } from 'lucide-react';
import { useTranslation } from '../i18n';

interface SharePermission {
  role: string;
  target_type: string;
  target: string;
}

interface Props {
  filePath: string;
  fileName: string;
  onClose: () => void;
}

const ROLES = [
  { value: 'reader', label: 'Viewer' },
  { value: 'writer', label: 'Editor' },
  { value: 'commenter', label: 'Commenter' },
];

export function SharePermissionsDialog({ filePath, fileName, onClose }: Props) {
  const t = useTranslation();
  const [permissions, setPermissions] = useState<SharePermission[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [newEmail, setNewEmail] = useState('');
  const [newRole, setNewRole] = useState('reader');
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<string | null>(null);

  // Hide scrollbars when dialog is open (WebKitGTK fix)
  useEffect(() => {
    document.documentElement.classList.add('modal-open');
    return () => { document.documentElement.classList.remove('modal-open'); };
  }, []);

  useEffect(() => {
    loadPermissions();
  }, [filePath]);

  const loadPermissions = async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<SharePermission[]>('provider_list_permissions', { path: filePath });
      setPermissions(result);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };

  const handleAdd = async () => {
    if (!newEmail.trim()) return;
    setAdding(true);
    try {
      await invoke('provider_add_permission', {
        path: filePath,
        role: newRole,
        targetType: 'user',
        target: newEmail.trim(),
      });
      setNewEmail('');
      await loadPermissions();
    } catch (err) {
      setError(String(err));
    } finally {
      setAdding(false);
    }
  };

  const handleRemove = async (target: string) => {
    setRemoving(target);
    try {
      await invoke('provider_remove_permission', { path: filePath, target });
      await loadPermissions();
    } catch (err) {
      setError(String(err));
    } finally {
      setRemoving(null);
    }
  };

  const roleLabel = (role: string) => {
    const found = ROLES.find(r => r.value === role);
    return found ? found.label : role;
  };

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={onClose}>
      <div
        className="bg-white dark:bg-gray-800 rounded-xl shadow-2xl w-[480px] max-h-[70vh] flex flex-col"
        onClick={e => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700">
          <div className="flex items-center gap-2">
            <Users size={18} className="text-blue-500" />
            <h3 className="font-semibold text-sm">{t('sharing.title') || 'Sharing'}</h3>
          </div>
          <button onClick={onClose} className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
            <X size={16} />
          </button>
        </div>

        {/* File name */}
        <div className="px-5 py-2 text-xs text-gray-500 dark:text-gray-400 truncate border-b border-gray-100 dark:border-gray-700">
          {fileName}
        </div>

        {/* Add permission form */}
        <div className="px-5 py-3 border-b border-gray-100 dark:border-gray-700 flex items-center gap-2">
          <input
            type="email"
            placeholder={t('sharing.email_placeholder') || 'Email address'}
            value={newEmail}
            onChange={e => setNewEmail(e.target.value)}
            onKeyDown={e => e.key === 'Enter' && handleAdd()}
            className="flex-1 text-sm px-3 py-1.5 rounded-lg border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 outline-none focus:ring-2 focus:ring-blue-500"
          />
          <select
            value={newRole}
            onChange={e => setNewRole(e.target.value)}
            className="text-sm px-2 py-1.5 rounded-lg border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 outline-none"
          >
            {ROLES.map(r => (
              <option key={r.value} value={r.value}>{r.label}</option>
            ))}
          </select>
          <button
            onClick={handleAdd}
            disabled={adding || !newEmail.trim()}
            className="px-3 py-1.5 bg-blue-500 hover:bg-blue-600 disabled:opacity-50 text-white text-sm rounded-lg flex items-center gap-1"
          >
            {adding ? <RefreshCw size={13} className="animate-spin" /> : <UserPlus size={13} />}
            {t('sharing.add') || 'Add'}
          </button>
        </div>

        {/* Permission list */}
        <div className="flex-1 overflow-auto">
          {loading ? (
            <div className="flex items-center justify-center py-12">
              <RefreshCw size={20} className="animate-spin text-blue-500" />
            </div>
          ) : error ? (
            <div className="p-5 text-sm text-red-500">{error}</div>
          ) : permissions.length === 0 ? (
            <div className="p-5 text-sm text-gray-400 text-center">{t('sharing.none') || 'No permissions set'}</div>
          ) : (
            <div className="divide-y divide-gray-100 dark:divide-gray-700">
              {permissions.map(p => (
                <div key={`${p.target_type}-${p.target}`} className="px-5 py-2.5 flex items-center justify-between hover:bg-gray-50 dark:hover:bg-gray-700/50">
                  <div className="flex-1 min-w-0">
                    <div className="text-sm truncate">{p.target || 'Anyone with link'}</div>
                    <div className="text-xs text-gray-400">{p.target_type} - {roleLabel(p.role)}</div>
                  </div>
                  {p.role !== 'owner' && (
                    <button
                      onClick={() => handleRemove(p.target)}
                      disabled={removing === p.target}
                      className="p-1 text-gray-400 hover:text-red-500 disabled:opacity-50 flex-shrink-0"
                      title={t('sharing.remove') || 'Remove'}
                    >
                      {removing === p.target ? <RefreshCw size={13} className="animate-spin" /> : <Trash2 size={13} />}
                    </button>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
