import { useState, useEffect, useRef } from 'react';

interface DebouncedSliderProps {
    value: number;
    onChange: (value: number) => void;
    min: number;
    max: number;
    step: number;
    className?: string; // For passing 'range range-purple range-xs' etc.
}

export default function DebouncedSlider({ value, onChange, min, max, step, className }: DebouncedSliderProps) {
    const [localValue, setLocalValue] = useState(value);
    const [isDragging, setIsDragging] = useState(false);
    const lastCommitted = useRef(value);

    // Sync local value with prop value when not dragging (for external updates)
    useEffect(() => {
        if (!isDragging) {
            setLocalValue(value);
            lastCommitted.current = value;
        }
    }, [value, isDragging]);

    const commit = (nextValue: number) => {
        if (lastCommitted.current === nextValue) return;
        lastCommitted.current = nextValue;
        onChange(nextValue);
    };

    const handleChange = (e: React.ChangeEvent<HTMLInputElement>) => {
        const nextValue = parseFloat(e.target.value);
        setLocalValue(nextValue);
        // Pointer interactions commit on release; keyboard changes have no pointer lifecycle.
        if (!isDragging) commit(nextValue);
    };

    const handlePointerDown = () => {
        setIsDragging(true);
    };

    const handlePointerUp = (e: React.PointerEvent<HTMLInputElement>) => {
        setIsDragging(false);
        commit(parseFloat((e.target as HTMLInputElement).value));
    };

    const handleBlur = () => {
        commit(localValue);
    };

    // Also handle onMouseUp/onTouchEnd as backup if Pointer events behave oddly in some envs, 
    // but Pointer events are standard now. 
    // Actually, simple onChange + onMouseUp is robust enough for standard ranges.

    return (
        <div className="flex items-center gap-3 w-full">
            <input
                type="range"
                min={min}
                max={max}
                step={step}
                className={className}
                value={localValue}
                onChange={handleChange}
                onBlur={handleBlur}
                onPointerDown={handlePointerDown}
                onPointerUp={handlePointerUp}
            />
            <span className="text-xs font-mono font-bold text-purple-600 dark:text-purple-400 w-10 text-right">
                {Math.round(localValue * 100)}%
            </span>
        </div>
    );
}
