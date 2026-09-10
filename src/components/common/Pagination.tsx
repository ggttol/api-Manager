import { ChevronLeft, ChevronRight } from 'lucide-react';
import { useTranslation } from 'react-i18next';

interface PaginationProps {
    currentPage: number;
    totalPages: number;
    onPageChange: (page: number) => void;
    totalItems: number;
    itemsPerPage: number;
    onPageSizeChange?: (pageSize: number) => void;  // 新增:分页大小变更回调
    pageSizeOptions?: number[];  // 新增:可选的分页大小选项
}

function Pagination({
    currentPage,
    totalPages,
    onPageChange,
    totalItems,
    itemsPerPage,
    onPageSizeChange,
    pageSizeOptions = [10, 20, 50, 100]
}: PaginationProps) {
    const { t } = useTranslation();

    if (totalPages <= 1 && !onPageSizeChange) return null;

    // 计算显示的页码范围 (最多显示 5 个页码)
    let startPage = Math.max(1, currentPage - 2);
    let endPage = Math.min(totalPages, startPage + 4);

    if (endPage - startPage < 4) {
        startPage = Math.max(1, endPage - 4);
    }

    const pages = [];
    for (let i = startPage; i <= endPage; i++) {
        pages.push(i);
    }

    const startIndex = totalItems === 0 ? 0 : (currentPage - 1) * itemsPerPage + 1;
    const endIndex = Math.min(currentPage * itemsPerPage, totalItems);

    return (
        <div className="console-toolbar justify-between py-3 shrink-0">
            <div className="console-toolbar text-xs console-muted">
                <p aria-live="polite">{t('common.pagination_info', { start: startIndex, end: endIndex, total: totalItems })}</p>
                {onPageSizeChange && <label className="flex items-center gap-2">
                    <span>{t('common.per_page')}</span>
                    <select value={itemsPerPage} onChange={event => onPageSizeChange(Number(event.target.value))}
                        className="select select-bordered select-sm">
                        {pageSizeOptions.map(size => <option key={size} value={size}>{size} {t('common.items')}</option>)}
                    </select>
                </label>}
            </div>
            <nav className="flex items-center gap-1" aria-label={t('common.per_page')}>
                <button className="console-button !px-2" onClick={() => onPageChange(currentPage - 1)}
                    disabled={currentPage <= 1} aria-label={t('common.prev_page')}>
                    <ChevronLeft size={16} aria-hidden="true" />
                </button>
                {startPage > 1 && <button className="console-button !px-3" onClick={() => onPageChange(1)}>1</button>}
                {startPage > 2 && <span className="px-1 console-muted" aria-hidden="true">…</span>}
                {pages.map(page => <button key={page} onClick={() => onPageChange(page)}
                    aria-current={page === currentPage ? 'page' : undefined}
                    className={`${page === currentPage ? 'console-button-primary' : 'console-button'} !px-3`}>
                    {page}
                </button>)}
                {endPage < totalPages - 1 && <span className="px-1 console-muted" aria-hidden="true">…</span>}
                {endPage < totalPages && <button className="console-button !px-3" onClick={() => onPageChange(totalPages)}>{totalPages}</button>}
                <button className="console-button !px-2" onClick={() => onPageChange(currentPage + 1)}
                    disabled={currentPage >= totalPages} aria-label={t('common.next_page')}>
                    <ChevronRight size={16} aria-hidden="true" />
                </button>
            </nav>
        </div>
    );
}

export default Pagination;
