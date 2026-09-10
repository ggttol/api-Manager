import { AlertTriangle, CheckCircle, XCircle, Info } from 'lucide-react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import { useEffect, useId, useRef } from 'react';

export type ModalType = 'confirm' | 'success' | 'error' | 'info';

interface ModalDialogProps {
    isOpen: boolean;
    title: string;
    message?: string;
    children?: React.ReactNode;
    type?: ModalType;
    onConfirm: () => void;
    onCancel?: () => void;
    confirmText?: string;
    cancelText?: string;
    isDestructive?: boolean;
}

export default function ModalDialog({
    isOpen,
    title,
    message,
    children,
    type = 'confirm',
    onConfirm,
    onCancel,
    confirmText,
    cancelText,
    isDestructive = false
}: ModalDialogProps) {
    const { t } = useTranslation();
    const finalConfirmText = confirmText || t('common.confirm');
    const finalCancelText = cancelText || t('common.cancel');
    const dialogRef = useRef<HTMLDialogElement>(null);
    const titleId = useId();

    useEffect(() => {
        const dialog = dialogRef.current;
        if (!isOpen || !dialog) return;
        dialog.showModal();
        return () => dialog.close();
    }, [isOpen]);

    if (!isOpen) return null;

    const getIcon = () => {
        switch (type) {
            case 'success':
                return <CheckCircle className="w-7 h-7 text-green-500" />;
            case 'error':
                return <XCircle className="w-7 h-7 text-red-500" />;
            case 'info':
                return <Info className="w-7 h-7 text-blue-500" />;
            case 'confirm':
            default:
                return isDestructive ? <AlertTriangle className="w-7 h-7 text-red-500" /> : <AlertTriangle className="w-7 h-7 text-blue-500" />;
        }
    };

    const getIconBg = () => {
        switch (type) {
            case 'success': return 'bg-green-50 dark:bg-green-900/20';
            case 'error': return 'bg-red-50 dark:bg-red-900/20';
            case 'info': return 'bg-blue-50 dark:bg-blue-900/20';
            case 'confirm': default: return isDestructive ? 'bg-red-50 dark:bg-red-900/20' : 'bg-blue-50 dark:bg-blue-900/20';
        }
    };

    const showCancel = type === 'confirm' && onCancel;

    return createPortal(
        <dialog ref={dialogRef} className="modal z-[100]" aria-labelledby={titleId}
            onCancel={event => { event.preventDefault(); if (showCancel) onCancel?.(); }}>
            <div className="modal-box relative max-w-sm console-panel !p-0 overflow-y-auto">
                <div className="flex flex-col items-center text-center p-6">
                    <div className={`w-12 h-12 rounded-xl flex items-center justify-center mb-4 ${getIconBg()}`}>
                        {getIcon()}
                    </div>

                    <h3 id={titleId} className="text-lg font-semibold mb-2">{title}</h3>

                    {children ? (
                        <div className="w-full text-left mb-8 px-1">
                            {children}
                        </div>
                    ) : (
                        <p className="text-gray-500 dark:text-gray-400 text-sm mb-8 leading-relaxed px-4">{message}</p>
                    )}

                    <div className="flex gap-3 w-full">
                        {showCancel && (
                            <button
                                className="console-button flex-1"
                                onClick={onCancel}
                            >
                                {finalCancelText}
                            </button>
                        )}
                        <button
                            className={`console-button-primary flex-1 ${isDestructive && type === 'confirm'
                                ? '!bg-red-600 !border-red-600 hover:!bg-red-700'
                                : ''}`}
                            onClick={onConfirm}
                        >
                            {finalConfirmText}
                        </button>
                    </div>
                </div>
            </div>
            <div className="modal-backdrop bg-black/40 fixed inset-0 z-[-1]" onClick={showCancel ? onCancel : undefined}></div>
        </dialog>,
        document.body
    );
}
