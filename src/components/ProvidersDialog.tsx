// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useEffect } from 'react';
import { X, Check, Minus } from 'lucide-react';
import { useTranslation } from '../i18n';
import { PROVIDER_LOGOS } from './ProviderLogos';

interface ProvidersDialogProps {
  isOpen: boolean;
  onClose: () => void;
}

interface ProviderFeatures {
  name: string;
  logoId: string;
  base: string[];
  advanced: string[];
  section?: string; // Section header before this provider
}

// Core operations: upload, download, delete, rename, move, mkdir, search (all providers)
const CORE_OPS = ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'] as const;

// Optional features (individual columns)
const OPTIONAL_FEATURES = [
  'shareLink', 'trash', 'versioning',
] as const;

// Extra features (accorpated in single "Extra" column with tooltip)
const EXTRA_FEATURES = ['star', 'comments', 'tags', 'collaborations', 'devTools'] as const;

// Premium features (accorpated in single "Premium" column with tooltip)
const PREMIUM_FEATURES = ['watermark', 'folderLock', 'filePassword'] as const;

// Enterprise features (accorpated in single "Enterprise" column with tooltip)
const ENTERPRISE_FEATURES = ['storageClass', 'objectTagging', 'sse', 'checksum', 'tierManagement'] as const;

// Labels for tooltip display
const FEATURE_LABELS: Record<string, string> = {
  star: 'Star', comments: 'Comments', tags: 'Tags',
  collaborations: 'Collaborations', devTools: 'Dev Tools',
  watermark: 'Watermark', folderLock: 'Folder Lock', filePassword: 'Password',
  storageClass: 'Storage Class', objectTagging: 'Object Tagging',
  sse: 'Server-Side Encryption', checksum: 'Checksum', tierManagement: 'Tier Management',
};

// All providers in a single flat list with section markers
const ALL_PROVIDERS: ProviderFeatures[] = [
  // ── OAuth / API ──
  { name: 'Google Drive', logoId: 'googledrive', section: 'OAuth / API',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: ['star', 'comments'] },
  { name: 'Dropbox', logoId: 'dropbox',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: [] },
  { name: 'OneDrive', logoId: 'onedrive',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: [] },
  { name: 'Box', logoId: 'box',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: ['comments', 'tags', 'collaborations', 'watermark', 'folderLock'] },
  { name: 'Zoho WorkDrive', logoId: 'zohoworkdrive',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: [] },
  { name: 'pCloud', logoId: 'pcloud',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: ['checksum'] },
  { name: 'MEGA', logoId: 'mega',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash'],
    advanced: [] },
  { name: 'Filen', logoId: 'filen',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'versioning'],
    advanced: [] },
  { name: 'Internxt', logoId: 'internxt',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'trash'],
    advanced: [] },
  { name: 'kDrive', logoId: 'kdrive',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: [] },
  { name: 'Jottacloud', logoId: 'jottacloud',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash'],
    advanced: [] },
  { name: 'FileLu', logoId: 'filelu',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'trash'],
    advanced: ['filePassword'] },
  { name: 'Koofr', logoId: 'koofr',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: [] },
  { name: 'OpenDrive', logoId: 'opendrive',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash'],
    advanced: [] },
  { name: 'Yandex Disk', logoId: 'yandexdisk',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: [] },
  { name: '4shared', logoId: '4shared',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash'],
    advanced: [] },
  { name: 'GitHub', logoId: 'github',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink'],
    advanced: ['devTools'] },

  // ── S3 Compatible ──
  { name: 'Amazon S3', logoId: 'amazon-s3', section: 'S3 Compatible',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'versioning'],
    advanced: ['storageClass', 'objectTagging', 'sse', 'checksum'] },
  { name: 'Backblaze B2', logoId: 'backblaze',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'Cloudflare R2', logoId: 'cloudflare-r2',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'Wasabi', logoId: 'wasabi',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'IDrive e2', logoId: 'idrive-e2',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'Storj', logoId: 'storj',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'DigitalOcean Spaces', logoId: 'digitalocean-spaces',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'Oracle Cloud', logoId: 'oracle-cloud',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'Alibaba OSS', logoId: 'alibaba-oss',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'Tencent COS', logoId: 'tencent-cos',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'MinIO', logoId: 'minio',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },

  // ── WebDAV ──
  { name: 'CloudMe', logoId: 'cloudme', section: 'WebDAV',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'DriveHQ', logoId: 'drivehq',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'Koofr (WebDAV)', logoId: 'koofr',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'Jianguoyun', logoId: 'jianguoyun',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'InfiniCloud', logoId: 'infinicloud',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'Seafile', logoId: 'seafile',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'FileLu WebDAV', logoId: 'filelu',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
  { name: 'Nextcloud', logoId: 'nextcloud',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search', 'shareLink', 'trash', 'versioning'],
    advanced: [] },
  { name: 'Hetzner Storage Box', logoId: 'hetzner',
    base: ['upload', 'download', 'delete', 'rename', 'move', 'mkdir', 'search'],
    advanced: [] },
];

// Helper: get extra/premium features a provider has
function getExtraFeatures(p: ProviderFeatures): string[] {
  return EXTRA_FEATURES.filter(f => p.advanced.includes(f)).map(f => FEATURE_LABELS[f] || f);
}
function getPremiumFeatures(p: ProviderFeatures): string[] {
  return PREMIUM_FEATURES.filter(f => p.advanced.includes(f)).map(f => FEATURE_LABELS[f] || f);
}
function getEnterpriseFeatures(p: ProviderFeatures): string[] {
  return ENTERPRISE_FEATURES.filter(f => p.advanced.includes(f)).map(f => FEATURE_LABELS[f] || f);
}

export function ProvidersDialog({ isOpen, onClose }: ProvidersDialogProps) {
  const t = useTranslation();

  useEffect(() => {
    if (isOpen) {
      document.documentElement.classList.add('modal-open');
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

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[3vh]">
      <div className="absolute inset-0 bg-black/50" onClick={onClose} />

      <div
        className="relative bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-xl shadow-2xl w-full max-w-[700px] overflow-hidden flex flex-col animate-scale-in"
        style={{ maxHeight: '90vh' }}
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

        {/* Legend */}
        <div className="flex items-center gap-4 px-5 py-2 border-b border-gray-200 dark:border-gray-700/50 shrink-0">
          <div className="flex items-center gap-1.5 text-[10px] text-gray-500 dark:text-gray-400">
            <Check size={12} className="text-emerald-500" />
            <span>Base</span>
          </div>
          <div className="flex items-center gap-1.5 text-[10px] text-gray-500 dark:text-gray-400">
            <Check size={12} className="text-blue-500" />
            <span>Advanced</span>
          </div>
          <div className="flex items-center gap-1.5 text-[10px] text-gray-500 dark:text-gray-400">
            <span className="text-[9px] font-bold bg-gradient-to-r from-amber-500 to-orange-500 text-white px-1 rounded">PRO</span>
            <span>Enterprise only</span>
          </div>
        </div>

        {/* Single unified table */}
        <div className="overflow-y-auto overflow-x-auto flex-1">
          <table className="w-full text-xs">
            <thead className="sticky top-0 bg-white dark:bg-gray-800 z-10">
              <tr className="border-b border-gray-200 dark:border-gray-700">
                <th className="text-left py-2 px-3 font-medium text-gray-500 dark:text-gray-400 min-w-[150px]">
                  {t('providers.provider')}
                </th>
                <th className="text-center py-2 px-2 font-medium text-gray-500 dark:text-gray-400 whitespace-nowrap">
                  Base
                </th>
                {OPTIONAL_FEATURES.map(f => (
                  <th key={f} className="text-center py-2 px-2 font-medium text-gray-500 dark:text-gray-400 whitespace-nowrap">
                    {t(`providers.${f}`)}
                  </th>
                ))}
                <th className="text-center py-2 px-2 font-medium text-gray-500 dark:text-gray-400 whitespace-nowrap">
                  Extra
                </th>
                <th className="text-center py-2 px-2 font-medium text-gray-500 dark:text-gray-400 whitespace-nowrap">
                  <span className="text-[9px] font-bold bg-gradient-to-r from-amber-500 to-orange-500 text-white px-1.5 py-0.5 rounded">PRO</span>
                </th>
                <th className="text-center py-2 px-2 font-medium text-gray-500 dark:text-gray-400 whitespace-nowrap">
                  <span className="text-[9px] font-bold bg-gradient-to-r from-cyan-500 to-blue-500 text-white px-1.5 py-0.5 rounded">S3/AZ</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {ALL_PROVIDERS.map((provider, idx) => {
                const Logo = PROVIDER_LOGOS[provider.logoId];
                const hasCoreOps = CORE_OPS.every(op => provider.base.includes(op));
                return (
                  <React.Fragment key={`${provider.logoId}-${idx}`}>
                    {provider.section && (
                      <tr>
                        <td
                          colSpan={2 + OPTIONAL_FEATURES.length + 3}
                          className="py-2 px-3 text-[10px] font-semibold text-gray-400 dark:text-gray-500 uppercase tracking-wider bg-gray-50 dark:bg-gray-800/80 border-t border-b border-gray-200 dark:border-gray-700"
                        >
                          {provider.section}
                        </td>
                      </tr>
                    )}
                    <tr className="border-b border-gray-200 dark:border-gray-700/30 hover:bg-gray-50 dark:hover:bg-gray-800/50 transition-colors">
                      <td className="py-1.5 px-3">
                        <div className="flex items-center gap-2">
                          <div className="w-5 h-5 flex-shrink-0 flex items-center justify-center">
                            {Logo ? <Logo size={16} /> : <div className="w-4 h-4 rounded bg-gray-400" />}
                          </div>
                          <span className="font-medium text-gray-900 dark:text-gray-100 whitespace-nowrap text-[11px]">{provider.name}</span>
                        </div>
                      </td>
                      <td className="text-center py-1.5 px-2" title={hasCoreOps ? 'Upload, Download, Delete, Rename, Move, Create Folder, Search' : ''}>
                        {hasCoreOps ? (
                          <Check size={13} className="inline-block text-emerald-500 cursor-help" />
                        ) : (
                          <Minus size={11} className="inline-block text-gray-400 dark:text-gray-600" />
                        )}
                      </td>
                      {OPTIONAL_FEATURES.map(f => (
                        <td key={f} className="text-center py-1.5 px-2">
                          {provider.base.includes(f) ? (
                            <span title={f === 'shareLink' && provider.logoId === 'github' ? 'Raw URL' : ''} className={f === 'shareLink' && provider.logoId === 'github' ? 'cursor-help' : ''}>
                              <Check size={13} className="inline-block text-emerald-500" />
                            </span>
                          ) : (
                            <Minus size={11} className="inline-block text-gray-400 dark:text-gray-600" />
                          )}
                        </td>
                      ))}
                      {/* Extra column */}
                      <td className="text-center py-1.5 px-2">
                        {(() => {
                          const extras = getExtraFeatures(provider);
                          return extras.length > 0 ? (
                            <span title={extras.join(', ')} className="cursor-help"><Check size={13} className="inline-block text-blue-500" /></span>
                          ) : (
                            <Minus size={11} className="inline-block text-gray-400 dark:text-gray-600" />
                          );
                        })()}
                      </td>
                      {/* Premium column */}
                      <td className="text-center py-1.5 px-2">
                        {(() => {
                          const prems = getPremiumFeatures(provider);
                          return prems.length > 0 ? (
                            <span title={prems.join(', ')} className="cursor-help">
                              <Check size={13} className="inline-block text-amber-500" />
                            </span>
                          ) : (
                            <Minus size={11} className="inline-block text-gray-400 dark:text-gray-600" />
                          );
                        })()}
                      </td>
                      {/* Enterprise S3/Azure column */}
                      <td className="text-center py-1.5 px-2">
                        {(() => {
                          const ent = getEnterpriseFeatures(provider);
                          return ent.length > 0 ? (
                            <span title={ent.join(', ')} className="cursor-help">
                              <Check size={13} className="inline-block text-cyan-500" />
                            </span>
                          ) : (
                            <Minus size={11} className="inline-block text-gray-400 dark:text-gray-600" />
                          );
                        })()}
                      </td>
                    </tr>
                  </React.Fragment>
                );
              })}
            </tbody>
          </table>
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-5 py-2 border-t border-gray-200 dark:border-gray-700 shrink-0">
          <p className="text-[10px] text-gray-400 dark:text-gray-500">
            Base = Upload, Download, Delete, Rename, Move, Create Folder, Search
          </p>
          <p className="text-[10px] text-gray-400 dark:text-gray-500">
            {ALL_PROVIDERS.length} integrated providers
          </p>
        </div>
      </div>
    </div>
  );
}
