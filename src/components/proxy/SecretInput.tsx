import { useEffect, useId, useState, type InputHTMLAttributes } from 'react';
import { useTranslation } from 'react-i18next';
import { Eye, EyeOff } from 'lucide-react';

export function useSecretVisibility(resetKey?: unknown) {
    const [revealed, setRevealed] = useState(false);

    useEffect(() => { setRevealed(false); }, [resetKey]);
    useEffect(() => {
        if (!revealed) return;
        const hide = () => setRevealed(false);
        const timer = window.setTimeout(hide, 30_000);
        window.addEventListener('blur', hide);
        document.addEventListener('visibilitychange', hide);
        return () => {
            window.clearTimeout(timer);
            window.removeEventListener('blur', hide);
            document.removeEventListener('visibilitychange', hide);
        };
    }, [revealed]);

    return { revealed, setRevealed };
}

export function SecretInput({ resetKey, ...props }: Omit<InputHTMLAttributes<HTMLInputElement>, 'type'> & { resetKey?: unknown }) {
    const { t, i18n } = useTranslation();
    const { revealed, setRevealed } = useSecretVisibility(resetKey);
    const generatedId = useId();
    const id = props.id || generatedId;
    const label = revealed
        ? t('console.hide_secret', { defaultValue: i18n.language.startsWith('zh') ? '隐藏敏感信息' : 'Hide secret' })
        : t('console.reveal_secret', { defaultValue: i18n.language.startsWith('zh') ? '显示 30 秒' : 'Reveal for 30 seconds' });

    return (
        <div className="flex min-w-0 flex-1 items-stretch gap-2" onBlur={(event) => {
            if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setRevealed(false);
        }}>
            <input {...props} id={id} type={revealed ? 'text' : 'password'} autoComplete="off" spellCheck={false} className={`min-w-0 w-full ${props.className || ''}`} />
            <button type="button" className="console-button shrink-0" aria-label={label} title={label} aria-controls={id} aria-pressed={revealed} onClick={(event) => { event.currentTarget.focus(); setRevealed(!revealed); }}>
                {revealed ? <EyeOff size={16} /> : <Eye size={16} />}
            </button>
        </div>
    );
}
