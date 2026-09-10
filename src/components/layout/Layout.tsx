import { Outlet } from 'react-router-dom';
import { getCurrentWindow } from '@tauri-apps/api/window';
import Navbar from '../navbar/Navbar';
import BackgroundTaskRunner from '../common/BackgroundTaskRunner';
import ToastContainer from '../common/ToastContainer';
import { useViewStore } from '../../stores/useViewStore';
import MiniView from './MiniView';
import { useEffect } from 'react';
import { isTauri } from '../../utils/env';
import { ensureFullViewState } from '../../utils/windowManager';

function Layout() {
    const { isMiniView } = useViewStore();

    // Ensure correct window state when in Full View (not Mini View)
    // This handles the case where the app was closed in Mini View (small size, no decorations)
    // and restarted (defaults to Full View state but keeps last window properties)
    useEffect(() => {
        if (!isMiniView && isTauri()) {
            ensureFullViewState();
        }
    }, [isMiniView]);

    if (isMiniView && isTauri()) {
        return (
            <>
                <BackgroundTaskRunner />
                <ToastContainer />
                <MiniView />
            </>
        );
    }

    return (
        <div className="console-window">
            {isTauri() && (
                <div
                    className="console-window-drag"
                    data-tauri-drag-region
                    onMouseDown={(event) => {
                        if (event.button === 0 && isTauri()) {
                            void getCurrentWindow().startDragging();
                        }
                    }}
                />
            )}
            <BackgroundTaskRunner />
            <ToastContainer />
            <Navbar>
                <Outlet />
            </Navbar>
        </div>
    );
}

export default Layout;
