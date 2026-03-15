import * as React from 'react';
import { useState, useEffect } from 'react';
import { X, Check, Minus } from 'lucide-react';
import { useTranslation } from '../i18n';
import { PROVIDER_LOGOS } from './ProviderLogos';

interface ProvidersDialogProps {
  isOpen: boolean;
  onClose: () => void;
}

type TabId = 'oauth' | 's3' | 'webdav';

interface ProviderFeatures {
  name: string;
  logoId: string;
  base: string[];
  advanced: string[];
}

const BASE_FEATURES = [
  'upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning',
] as const;

const ADVANCED_FEATURES = [
  'star', 'comments', 'tags', 'collaborations', 'watermark', 'folderLock', 'filePassword',
] as const;

type BaseFeature = typeof BASE_FEATURES[number];
type AdvancedFeature = typeof ADVANCED_FEATURES[number];

// Static feature map — reflects actually implemented features
const OAUTH_PROVIDERS: ProviderFeatures[] = [
  {
    name: 'Google Drive',
    logoId: 'googledrive',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: ['star', 'comments'],
  },
  {
    name: 'Dropbox',
    logoId: 'dropbox',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash'],
    advanced: [],
  },
  {
    name: 'OneDrive',
    logoId: 'onedrive',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'versioning'],
    advanced: [],
  },
  {
    name: 'Box',
    logoId: 'box',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash'],
    advanced: ['comments', 'tags', 'collaborations', 'watermark', 'folderLock'],
  },
  {
    name: 'pCloud',
    logoId: 'pcloud',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash'],
    advanced: [],
  },
  {
    name: 'MEGA',
    logoId: 'mega',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'trash'],
    advanced: [],
  },
  {
    name: 'Filen',
    logoId: 'filen',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: [],
  },
  {
    name: 'Internxt',
    logoId: 'internxt',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'trash'],
    advanced: [],
  },
  {
    name: 'kDrive',
    logoId: 'kdrive',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash'],
    advanced: [],
  },
  {
    name: 'Jottacloud',
    logoId: 'jottacloud',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'trash'],
    advanced: [],
  },
  {
    name: 'Zoho WorkDrive',
    logoId: 'zohoworkdrive',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: [],
  },
  {
    name: 'FileLu',
    logoId: 'filelu',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'trash'],
    advanced: ['filePassword'],
  },
  {
    name: 'Koofr',
    logoId: 'koofr',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: [],
  },
  {
    name: 'Yandex Disk',
    logoId: 'yandexdisk',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: [],
  },
];

const S3_PROVIDERS: ProviderFeatures[] = [
  { name: 'Amazon S3', logoId: 'amazon-s3', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'Backblaze B2', logoId: 'backblaze', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'Cloudflare R2', logoId: 'cloudflare-r2', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'Wasabi', logoId: 'wasabi', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'IDrive e2', logoId: 'idrive-e2', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'Storj', logoId: 'storj', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'DigitalOcean Spaces', logoId: 'digitalocean-spaces', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'Oracle Cloud', logoId: 'oracle-cloud', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'Alibaba OSS', logoId: 'alibaba-oss', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'Tencent COS', logoId: 'tencent-cos', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'MinIO', logoId: 'minio', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
];

const WEBDAV_PROVIDERS: ProviderFeatures[] = [
  { name: 'Nextcloud', logoId: 'nextcloud', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'], advanced: [] },
  { name: '4shared', logoId: '4shared', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash'], advanced: [] },
  { name: 'CloudMe', logoId: 'cloudme', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'DriveHQ', logoId: 'drivehq', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'Koofr (WebDAV)', logoId: 'koofr', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'Jianguoyun', logoId: 'jianguoyun', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'InfiniCloud', logoId: 'infinicloud', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
  { name: 'Seafile', logoId: 'seafile', base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir'], advanced: [] },
];

const PRO_FEATURES = new Set<string>(['watermark', 'folderLock']);

function FeatureTable({ providers, t }: { providers: ProviderFeatures[]; t: (k: string) => string }) {
  const hasAnyAdvanced = providers.some(p => p.advanced.length > 0);

  return (
    <div className="overflow-x-auto">
      <table className="w-full text-xs">
        <thead>
          <tr className="border-b border-gray-200 dark:border-gray-700">
            <th className="text-left py-2 px-2 font-medium text-gray-500 dark:text-gray-400 sticky left-0 bg-white dark:bg-gray-800 min-w-[160px]">
              {t('providers.provider')}
            </th>
            {BASE_FEATURES.map(f => (
              <th key={f} className="text-center py-2 px-1 font-medium text-gray-500 dark:text-gray-400 whitespace-nowrap" title={t(`providers.${f}`)}>
                <span className="inline-block max-w-[76px] truncate">{t(`providers.${f}`)}</span>
              </th>
            ))}
            {hasAnyAdvanced && ADVANCED_FEATURES.map(f => (
              <th key={f} className="text-center py-2 px-1 font-medium text-gray-500 dark:text-gray-400 whitespace-nowrap" title={t(`providers.${f}`)}>
                <span className="inline-block max-w-[76px] truncate">{t(`providers.${f}`)}</span>
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {providers.map(provider => {
            const Logo = PROVIDER_LOGOS[provider.logoId];
            return (
              <tr key={provider.logoId} className="border-b border-gray-200 dark:border-gray-700/50 hover:bg-gray-50 dark:hover:bg-gray-800/50 transition-colors">
                <td className="py-2 px-2 sticky left-0 bg-white dark:bg-gray-800">
                  <div className="flex items-center gap-2">
                    <div className="w-5 h-5 flex-shrink-0 flex items-center justify-center">
                      {Logo ? <Logo size={18} /> : <div className="w-4 h-4 rounded bg-gray-400" />}
                    </div>
                    <span className="font-medium text-gray-900 dark:text-gray-100 whitespace-nowrap">{provider.name}</span>
                  </div>
                </td>
                {BASE_FEATURES.map(f => (
                  <td key={f} className="text-center py-2 px-1">
                    {provider.base.includes(f) ? (
                      <Check size={14} className="inline-block text-emerald-500" />
                    ) : (
                      <Minus size={12} className="inline-block text-gray-400 dark:text-gray-600" />
                    )}
                  </td>
                ))}
                {hasAnyAdvanced && ADVANCED_FEATURES.map(f => (
                  <td key={f} className="text-center py-2 px-1">
                    {provider.advanced.includes(f) ? (
                      <span className="inline-flex items-center">
                        <Check size={14} className="text-blue-500" />
                        {PRO_FEATURES.has(f) && (
                          <span className="ml-0.5 text-[9px] font-bold bg-gradient-to-r from-amber-500 to-orange-500 text-white px-1 rounded leading-tight">PRO</span>
                        )}
                      </span>
                    ) : (
                      <Minus size={12} className="inline-block text-gray-400 dark:text-gray-600" />
                    )}
                  </td>
                ))}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

export function ProvidersDialog({ isOpen, onClose }: ProvidersDialogProps) {
  const t = useTranslation();
  const [activeTab, setActiveTab] = useState<TabId>('oauth');

  useEffect(() => {
    if (isOpen) {
      document.documentElement.classList.add('modal-open');
      setActiveTab('oauth');
      return () => { document.documentElement.classList.remove('modal-open'); };
    }
  }, [isOpen]);

  useEffect(() => {
    if (!isOpen) return;
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [isOpen, onClose]);

  if (!isOpen) return null;

  const tabs: { id: TabId; label: string; count: number }[] = [
    { id: 'oauth', label: t('providers.tabOAuth'), count: OAUTH_PROVIDERS.length },
    { id: 's3', label: t('providers.tabS3'), count: S3_PROVIDERS.length },
    { id: 'webdav', label: t('providers.tabWebDAV'), count: WEBDAV_PROVIDERS.length },
  ];

  const currentProviders = activeTab === 'oauth' ? OAUTH_PROVIDERS : activeTab === 's3' ? S3_PROVIDERS : WEBDAV_PROVIDERS;

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh]">
      <div className="absolute inset-0 bg-black/50" onClick={onClose} />

      <div
        className="relative bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-xl shadow-2xl w-full max-w-[1140px] overflow-hidden flex flex-col animate-scale-in"
        style={{ maxHeight: '85vh' }}
        role="dialog"
        aria-modal="true"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700 shrink-0">
          <div className="flex items-center gap-2.5">
            <img src="/icons/AeroFTP_simbol_color_512x512.png" alt="AeroFTP" className="w-6 h-6 object-contain" />
            <h2 className="text-base font-semibold text-gray-900 dark:text-gray-100">
              {t('providers.title')}
            </h2>
          </div>
          <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700" title={t('common.close')}>
            <X size={16} className="text-gray-500 dark:text-gray-400" />
          </button>
        </div>

        {/* Tab bar */}
        <div className="flex border-b border-gray-200 dark:border-gray-700 shrink-0">
          {tabs.map(tab => (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={`flex-1 px-4 py-2.5 text-sm font-medium transition-colors relative ${
                activeTab === tab.id
                  ? 'text-blue-500 dark:text-cyan-400'
                  : 'text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-300'
              }`}
            >
              {tab.label}
              <span className="ml-1.5 text-[10px] opacity-60">({tab.count})</span>
              {activeTab === tab.id && (
                <div className="absolute bottom-0 left-0 right-0 h-[2px] bg-blue-500 dark:bg-cyan-400" />
              )}
            </button>
          ))}
        </div>

        {/* Legend */}
        <div className="flex items-center gap-4 px-5 py-2 border-b border-gray-200 dark:border-gray-700/50 shrink-0">
          <div className="flex items-center gap-1.5 text-[10px] text-gray-500 dark:text-gray-400">
            <Check size={12} className="text-emerald-500" />
            <span>{t('providers.baseFeatures')}</span>
          </div>
          <div className="flex items-center gap-1.5 text-[10px] text-gray-500 dark:text-gray-400">
            <Check size={12} className="text-blue-500" />
            <span>{t('providers.proFeatures')}</span>
          </div>
          <div className="flex items-center gap-1.5 text-[10px] text-gray-500 dark:text-gray-400">
            <span className="text-[9px] font-bold bg-gradient-to-r from-amber-500 to-orange-500 text-white px-1 rounded leading-tight">PRO</span>
            <span>{t('providers.enterpriseOnly')}</span>
          </div>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-auto min-h-0 px-2 py-1">
          <FeatureTable providers={currentProviders} t={t} />
        </div>

        {/* Footer */}
        <div className="px-5 py-2.5 border-t border-gray-200 dark:border-gray-700 shrink-0 text-center">
          <span className="text-[10px] text-gray-400 dark:text-gray-600">
            {t('providers.totalCount', { count: OAUTH_PROVIDERS.length + S3_PROVIDERS.length + WEBDAV_PROVIDERS.length })}
          </span>
        </div>
      </div>
    </div>
  );
}
