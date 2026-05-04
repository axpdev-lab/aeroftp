// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React from 'react';
import { Globe, HardDrive, ArrowLeftRight } from 'lucide-react';

interface PanelSwitcherProps {
  activePanel: 'remote' | 'local';
  swapPanels: boolean;
  onPanelSelect: (panel: 'remote' | 'local') => void;
  onSwap: () => void;
  remoteLabel: string;
  localLabel: string;
  swapTitle: string;
}

const PanelSwitcher: React.FC<PanelSwitcherProps> = ({
  activePanel,
  swapPanels,
  onPanelSelect,
  onSwap,
  remoteLabel,
  localLabel,
  swapTitle,
}) => {
  const activeClass = 'bg-blue-500 text-white';
  const inactiveClass = 'bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500';

  const remoteBtn = (
    <button
      onClick={() => onPanelSelect('remote')}
      className={`px-4 py-1.5 rounded-lg text-sm flex items-center gap-1.5 transition-colors ${activePanel === 'remote' ? activeClass : inactiveClass}`}
    >
      <Globe size={16} /> {remoteLabel}
    </button>
  );

  const localBtn = (
    <button
      onClick={() => onPanelSelect('local')}
      className={`px-4 py-1.5 rounded-lg text-sm flex items-center gap-1.5 transition-colors ${activePanel === 'local' ? activeClass : inactiveClass}`}
    >
      <HardDrive size={16} /> {localLabel}
    </button>
  );

  const swapBtn = (
    <button
      onClick={onSwap}
      className={`px-2.5 py-1.5 rounded-lg text-sm flex items-center transition-colors ${swapPanels ? activeClass : inactiveClass}`}
      title={swapTitle}
    >
      <ArrowLeftRight size={16} />
    </button>
  );

  // Left button matches the left panel, right button matches the right panel
  const leftBtn = swapPanels ? localBtn : remoteBtn;
  const rightBtn = swapPanels ? remoteBtn : localBtn;

  return (
    <div className="flex items-center gap-1.5">
      {leftBtn}
      {swapBtn}
      {rightBtn}
    </div>
  );
};

export default React.memo(PanelSwitcher);
