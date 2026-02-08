import React, { useState, useRef, useEffect } from 'react';
import { GitBranch, ChevronDown, X } from 'lucide-react';
import { useTranslation } from '../../i18n';

interface ConversationBranchInfo {
    id: string;
    name: string;
    messageCount: number;
    createdAt: string;
}

interface ConversationBranchProps {
    branches: ConversationBranchInfo[];
    activeBranchId: string | null;  // null = main conversation
    onSwitchBranch: (branchId: string | null) => void;
    onDeleteBranch: (branchId: string) => void;
}

/** Branch selector dropdown in chat header */
export const BranchSelector: React.FC<ConversationBranchProps> = ({
    branches,
    activeBranchId,
    onSwitchBranch,
    onDeleteBranch,
}) => {
    const [open, setOpen] = useState(false);
    const dropdownRef = useRef<HTMLDivElement>(null);
    const t = useTranslation();

    // Close on outside click
    useEffect(() => {
        if (!open) return;
        const handler = (e: MouseEvent) => {
            if (dropdownRef.current && !dropdownRef.current.contains(e.target as Node)) {
                setOpen(false);
            }
        };
        document.addEventListener('mousedown', handler);
        return () => document.removeEventListener('mousedown', handler);
    }, [open]);

    if (branches.length === 0) return null;

    const activeName = activeBranchId
        ? branches.find(b => b.id === activeBranchId)?.name || 'Branch'
        : (t('ai.branch.main') || 'Main');

    return (
        <div className="relative" ref={dropdownRef}>
            <button
                onClick={() => setOpen(!open)}
                className="flex items-center gap-1 px-2 py-1 text-xs text-gray-400 hover:text-gray-200 rounded hover:bg-gray-700/50 transition-colors"
                title={t('ai.branch.switchBranch') || 'Switch branch'}
            >
                <GitBranch size={12} />
                <span className="max-w-[100px] truncate">{activeName}</span>
                <span className="text-gray-600 text-[10px]">({branches.length + 1})</span>
                <ChevronDown size={10} />
            </button>

            {open && (
                <div className="absolute top-full left-0 mt-1 w-56 bg-gray-800 border border-gray-700 rounded-lg shadow-xl z-50 overflow-hidden">
                    {/* Main conversation */}
                    <button
                        onClick={() => { onSwitchBranch(null); setOpen(false); }}
                        className={`w-full text-left px-3 py-2 text-xs flex items-center gap-2 hover:bg-gray-700/50 ${
                            !activeBranchId ? 'bg-gray-700/30 text-white' : 'text-gray-300'
                        }`}
                    >
                        <GitBranch size={11} className="text-green-400" />
                        <span className="font-medium">{t('ai.branch.main') || 'Main'}</span>
                    </button>

                    {/* Branches */}
                    {branches.map(branch => (
                        <div
                            key={branch.id}
                            className={`flex items-center gap-1 px-3 py-2 text-xs hover:bg-gray-700/50 ${
                                activeBranchId === branch.id ? 'bg-gray-700/30 text-white' : 'text-gray-300'
                            }`}
                        >
                            <button
                                onClick={() => { onSwitchBranch(branch.id); setOpen(false); }}
                                className="flex-1 flex items-center gap-2 text-left"
                            >
                                <GitBranch size={11} className="text-purple-400" />
                                <span className="truncate">{branch.name}</span>
                                <span className="text-gray-600 text-[10px]">{branch.messageCount} msg</span>
                            </button>
                            <button
                                onClick={(e) => { e.stopPropagation(); onDeleteBranch(branch.id); }}
                                className="p-0.5 text-gray-600 hover:text-red-400 transition-colors"
                                title={t('ai.branch.delete') || 'Delete branch'}
                            >
                                <X size={10} />
                            </button>
                        </div>
                    ))}
                </div>
            )}
        </div>
    );
};

export default BranchSelector;
