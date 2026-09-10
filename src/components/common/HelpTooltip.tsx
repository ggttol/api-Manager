import { CircleHelp } from 'lucide-react';
import { useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';

export type HelpTooltipPlacement = 'top' | 'right' | 'bottom' | 'left';

export type HelpTooltipProps = {
    text: string;
    placement?: HelpTooltipPlacement;
    ariaLabel?: string;
    iconSize?: number;
    className?: string;
};


export default function HelpTooltip({
    text,
    placement = 'top',
    ariaLabel = 'Help',
    iconSize = 14,
    className,
}: HelpTooltipProps) {
    const [open, setOpen] = useState(false);
    const [position, setPosition] = useState({ left: 16, top: 16 });
    const trigger = useRef<HTMLButtonElement>(null);
    const tooltip = useRef<HTMLSpanElement>(null);
    const id = useId();

    useLayoutEffect(() => {
        if (!open || !trigger.current || !tooltip.current) return;
        const anchor = trigger.current.getBoundingClientRect();
        const box = tooltip.current.getBoundingClientRect();
        let left = anchor.left + (anchor.width - box.width) / 2;
        let top = placement === 'bottom' ? anchor.bottom + 8 : anchor.top - box.height - 8;
        if (placement === 'left' || placement === 'right') {
            left = placement === 'right' ? anchor.right + 8 : anchor.left - box.width - 8;
            top = anchor.top + (anchor.height - box.height) / 2;
        }
        if (top < 16) top = anchor.bottom + 8;
        setPosition({
            left: Math.max(16, Math.min(left, window.innerWidth - box.width - 16)),
            top: Math.max(16, Math.min(top, window.innerHeight - box.height - 16)),
        });
    }, [open, placement, text]);

    useEffect(() => {
        if (!open) return;
        const close = () => setOpen(false);
        const outside = (event: PointerEvent) => {
            if (!trigger.current?.contains(event.target as Node) && !tooltip.current?.contains(event.target as Node)) close();
        };
        window.addEventListener('resize', close);
        window.addEventListener('scroll', close, true);
        document.addEventListener('pointerdown', outside);
        return () => {
            window.removeEventListener('resize', close);
            window.removeEventListener('scroll', close, true);
            document.removeEventListener('pointerdown', outside);
        };
    }, [open]);

    if (!text) return null;
    return (
        <span className={`inline-flex items-center ${className || ''}`}>
            <button ref={trigger} type="button"
                className="inline-flex items-center justify-center console-muted hover:text-[var(--console-text)] rounded"
                aria-label={ariaLabel} aria-describedby={open ? id : undefined}
                onMouseEnter={() => setOpen(true)} onMouseLeave={() => setOpen(false)}
                onFocus={() => setOpen(true)} onBlur={() => setOpen(false)}
                onKeyDown={event => { if (event.key === 'Escape') { event.stopPropagation(); setOpen(false); } }}
                onClick={event => { event.preventDefault(); event.stopPropagation(); setOpen(true); }}>
                <CircleHelp size={iconSize} />
            </button>
            {open && createPortal(<span ref={tooltip} id={id} role="tooltip"
                className="fixed z-[150] w-80 max-w-[calc(100vw-32px)] rounded-lg bg-slate-900 text-white text-xs leading-relaxed p-3 shadow-lg"
                style={position}>{text}</span>, document.body)}
        </span>
    );
}
