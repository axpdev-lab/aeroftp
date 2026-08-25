// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useEffect, useState } from 'react';
import { HardDrive } from 'lucide-react';

export type ManualStorageUnit = 'MB' | 'GB' | 'TB' | 'PB';

const UNIT_BYTES: Record<ManualStorageUnit, number> = {
    MB: 1024 ** 2,
    GB: 1024 ** 3,
    TB: 1024 ** 4,
    PB: 1024 ** 5,
};

export function manualStorageBytes(amount: string, unit: ManualStorageUnit): number | undefined {
    if (!amount.trim()) return undefined;
    const numeric = Number(amount);
    if (!Number.isFinite(numeric) || numeric <= 0) return undefined;
    const bytes = Math.round(numeric * UNIT_BYTES[unit]);
    return Number.isSafeInteger(bytes) && bytes > 0 ? bytes : undefined;
}

export function manualStorageInput(bytes?: number): { amount: string; unit: ManualStorageUnit } {
    if (!bytes || !Number.isFinite(bytes) || bytes <= 0) return { amount: '', unit: 'GB' };
    const units: ManualStorageUnit[] = ['PB', 'TB', 'GB', 'MB'];
    const unit = units.find(candidate => bytes >= UNIT_BYTES[candidate]) || 'MB';
    const value = bytes / UNIT_BYTES[unit];
    return {
        amount: Number.isInteger(value) ? String(value) : String(Number(value.toPrecision(12))),
        unit,
    };
}

interface ManualStorageFieldProps {
    valueBytes?: number;
    onChange: (bytes: number | undefined) => void;
    label: string;
    hint: string;
    disabled?: boolean;
}

/**
 * Numeric manual-quota editor. The amount is deliberately unrestricted above
 * 1024 and accepts decimals; the explicit unit removes the ambiguity of a free
 * text size while preserving every value already stored as bytes (#369).
 */
export const ManualStorageField: React.FC<ManualStorageFieldProps> = ({
    valueBytes,
    onChange,
    label,
    hint,
    disabled = false,
}) => {
    const initial = manualStorageInput(valueBytes);
    const [amount, setAmount] = useState(initial.amount);
    const [unit, setUnit] = useState<ManualStorageUnit>(initial.unit);

    useEffect(() => {
        const current = manualStorageBytes(amount, unit);
        if (current === valueBytes || (!current && !valueBytes)) return;
        const next = manualStorageInput(valueBytes);
        setAmount(next.amount);
        setUnit(next.unit);
    }, [valueBytes]); // eslint-disable-line react-hooks/exhaustive-deps

    const commit = (nextAmount: string, nextUnit: ManualStorageUnit) => {
        onChange(manualStorageBytes(nextAmount, nextUnit));
    };

    return (
        <div>
            <label className="block text-sm font-medium mb-1.5 flex items-center gap-1.5">
                <HardDrive size={14} />
                {label}
            </label>
            <div className="flex gap-2">
                <input
                    type="number"
                    inputMode="decimal"
                    min="0"
                    step="any"
                    value={amount}
                    onChange={(event) => {
                        const next = event.target.value;
                        setAmount(next);
                        commit(next, unit);
                    }}
                    disabled={disabled}
                    className="min-w-0 flex-1 px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                />
                <select
                    value={unit}
                    onChange={(event) => {
                        const next = event.target.value as ManualStorageUnit;
                        setUnit(next);
                        commit(amount, next);
                    }}
                    disabled={disabled}
                    aria-label={`${label} unit`}
                    className="w-24 px-3 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                >
                    {(['MB', 'GB', 'TB', 'PB'] as ManualStorageUnit[]).map(option => (
                        <option key={option} value={option}>{option}</option>
                    ))}
                </select>
            </div>
            <p className="mt-1 text-xs text-gray-400 dark:text-gray-500">{hint}</p>
        </div>
    );
};

export default ManualStorageField;
