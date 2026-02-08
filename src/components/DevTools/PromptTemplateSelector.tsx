import React, { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { PromptTemplate, matchTemplates } from './aiChatPromptTemplates';

interface PromptTemplateSelectorProps {
    input: string;
    templates: PromptTemplate[];
    onSelect: (template: PromptTemplate) => void;
    onDismiss: () => void;
    visible: boolean;
}

const CATEGORY_LABELS: Record<PromptTemplate['category'], string> = {
    code: 'Code',
    debug: 'Debug',
    docs: 'Documentation',
    security: 'Security',
    analysis: 'Analysis',
    general: 'General',
};

const CATEGORY_ORDER: PromptTemplate['category'][] = [
    'code',
    'debug',
    'docs',
    'security',
    'analysis',
    'general',
];

/**
 * Group templates by category, preserving a stable category order.
 */
function groupByCategory(
    templates: PromptTemplate[]
): { category: PromptTemplate['category']; label: string; items: PromptTemplate[] }[] {
    const map = new Map<PromptTemplate['category'], PromptTemplate[]>();
    for (const t of templates) {
        const list = map.get(t.category) || [];
        list.push(t);
        map.set(t.category, list);
    }
    const groups: { category: PromptTemplate['category']; label: string; items: PromptTemplate[] }[] = [];
    for (const cat of CATEGORY_ORDER) {
        const items = map.get(cat);
        if (items && items.length > 0) {
            groups.push({ category: cat, label: CATEGORY_LABELS[cat], items });
        }
    }
    return groups;
}

const PromptTemplateSelector: React.FC<PromptTemplateSelectorProps> = React.memo(({
    input,
    templates,
    onSelect,
    onDismiss,
    visible,
}) => {
    const [selectedIndex, setSelectedIndex] = useState(0);
    const listRef = useRef<HTMLDivElement>(null);
    const itemRefs = useRef<Map<number, HTMLDivElement>>(new Map());

    const filtered = useMemo(() => matchTemplates(input, templates), [input, templates]);

    const showCategories = useMemo(() => {
        const query = input.slice(1).toLowerCase();
        return !query;
    }, [input]);

    const groups = useMemo(() => {
        if (!showCategories) return null;
        return groupByCategory(filtered);
    }, [filtered, showCategories]);

    // Flat list of templates for index-based navigation
    const flatList = useMemo(() => {
        if (groups) {
            const flat: PromptTemplate[] = [];
            for (const g of groups) {
                flat.push(...g.items);
            }
            return flat;
        }
        return filtered;
    }, [groups, filtered]);

    // Reset selection when filtered list changes
    useEffect(() => {
        setSelectedIndex(0);
    }, [filtered.length, input]);

    // Scroll selected item into view
    useEffect(() => {
        const el = itemRefs.current.get(selectedIndex);
        if (el) {
            el.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
        }
    }, [selectedIndex]);

    // Keyboard handler — attached to document so parent can delegate
    const handleKeyDown = useCallback(
        (e: KeyboardEvent) => {
            if (!visible || flatList.length === 0) return;

            switch (e.key) {
                case 'ArrowDown':
                    e.preventDefault();
                    e.stopPropagation();
                    setSelectedIndex(prev => (prev + 1) % flatList.length);
                    break;
                case 'ArrowUp':
                    e.preventDefault();
                    e.stopPropagation();
                    setSelectedIndex(prev => (prev - 1 + flatList.length) % flatList.length);
                    break;
                case 'Enter':
                    e.preventDefault();
                    e.stopPropagation();
                    onSelect(flatList[selectedIndex]);
                    break;
                case 'Escape':
                    e.preventDefault();
                    e.stopPropagation();
                    onDismiss();
                    break;
                case 'Tab':
                    e.preventDefault();
                    e.stopPropagation();
                    onSelect(flatList[selectedIndex]);
                    break;
                default:
                    break;
            }
        },
        [visible, flatList, selectedIndex, onSelect, onDismiss]
    );

    useEffect(() => {
        if (!visible) return;
        document.addEventListener('keydown', handleKeyDown, true);
        return () => document.removeEventListener('keydown', handleKeyDown, true);
    }, [visible, handleKeyDown]);

    // Store ref for each item by flat index
    const setItemRef = useCallback((index: number, el: HTMLDivElement | null) => {
        if (el) {
            itemRefs.current.set(index, el);
        } else {
            itemRefs.current.delete(index);
        }
    }, []);

    if (!visible || flatList.length === 0) return null;

    // Build a flat-index counter to map grouped rendering back to flatList indices
    let flatIndex = 0;

    const renderItem = (template: PromptTemplate, idx: number) => {
        const currentFlatIndex = idx;
        const isSelected = currentFlatIndex === selectedIndex;

        return (
            <div
                key={template.id}
                ref={(el) => setItemRef(currentFlatIndex, el)}
                className={`flex items-center gap-2 px-3 py-1.5 cursor-pointer transition-colors duration-100 ${
                    isSelected
                        ? 'bg-blue-600 bg-opacity-40 text-white'
                        : 'text-gray-300 hover:bg-gray-700'
                }`}
                onMouseEnter={() => setSelectedIndex(currentFlatIndex)}
                onMouseDown={(e) => {
                    e.preventDefault();
                    onSelect(template);
                }}
            >
                <span className="text-sm flex-shrink-0 w-5 text-center">{template.icon}</span>
                <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                        <span className="text-xs font-medium truncate">{template.name}</span>
                        <span className="text-xs text-gray-500 flex-shrink-0">{template.command}</span>
                    </div>
                    <p className="text-xs text-gray-400 truncate">{template.description}</p>
                </div>
            </div>
        );
    };

    return (
        <div
            className="absolute bottom-full left-0 right-0 mb-1 z-50 min-w-[280px] max-h-64 overflow-y-auto bg-gray-800 border border-gray-600 rounded-lg shadow-xl"
            ref={listRef}
        >
            {showCategories && groups ? (
                <>
                    {groups.map((group) => {
                        const items = group.items.map((t, i) => {
                            const rendered = renderItem(t, flatIndex);
                            flatIndex++;
                            return rendered;
                        });
                        return (
                            <div key={group.category}>
                                <div className="px-3 py-1 text-xs font-semibold text-gray-500 uppercase tracking-wider bg-gray-850 sticky top-0 bg-gray-800 border-b border-gray-700">
                                    {group.label}
                                </div>
                                {items}
                            </div>
                        );
                    })}
                </>
            ) : (
                flatList.map((t, i) => renderItem(t, i))
            )}
        </div>
    );
});

PromptTemplateSelector.displayName = 'PromptTemplateSelector';

export default PromptTemplateSelector;
