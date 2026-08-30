let dashboardRefreshInProgress = false;

function reconcileDashboardNode(current, fresh) {
    if (current.nodeType !== fresh.nodeType || current.nodeName !== fresh.nodeName) {
        current.replaceWith(fresh.cloneNode(true));
        return;
    }
    if (current.nodeType === Node.TEXT_NODE) {
        if (current.data !== fresh.data) current.data = fresh.data;
        return;
    }
    if (
        current.nodeType !== Node.ELEMENT_NODE
        || current.matches('[data-client-state],script')
    ) return;

    for (const attribute of [...current.attributes]) {
        if (!fresh.hasAttribute(attribute.name)) current.removeAttribute(attribute.name);
    }
    for (const attribute of [...fresh.attributes]) {
        if (current.getAttribute(attribute.name) !== attribute.value) {
            current.setAttribute(attribute.name, attribute.value);
        }
    }

    let currentChild = current.firstChild;
    let freshChild = fresh.firstChild;
    while (currentChild || freshChild) {
        if (!currentChild) {
            current.append(freshChild.cloneNode(true));
            freshChild = freshChild.nextSibling;
            continue;
        }
        if (!freshChild) {
            const next = currentChild.nextSibling;
            currentChild.remove();
            currentChild = next;
            continue;
        }
        const nextCurrent = currentChild.nextSibling;
        const nextFresh = freshChild.nextSibling;
        reconcileDashboardNode(currentChild, freshChild);
        currentChild = nextCurrent;
        freshChild = nextFresh;
    }
}

async function refreshDashboard() {
    if (dashboardRefreshInProgress) return;
    dashboardRefreshInProgress = true;
    try {
        const response = await fetch('/', { cache: 'no-store' });
        if (!response.ok) throw new Error('dashboard refresh failed');
        const fresh = new DOMParser().parseFromString(await response.text(), 'text/html');
        const currentShell = document.querySelector('.shell');
        const freshShell = fresh.querySelector('.shell');
        if (!currentShell || !freshShell) throw new Error('dashboard content missing');
        reconcileDashboardNode(currentShell, freshShell);
        document.body.className = fresh.body.className;
    } catch (_) {
        // Keep the last usable dashboard visible; the next interval retries.
    } finally {
        dashboardRefreshInProgress = false;
    }
}

async function sendTestNotification(button) {
    const status = document.getElementById('test-feedback');
    button.disabled = true;
    status.className = 'test-feedback sending';
    status.textContent = 'Sending';
    try {
        const response = await fetch('/api/v1/notifications/test', { method: 'POST' });
        if (!response.ok) throw new Error('test notification failed');
        status.className = 'test-feedback ok';
        status.textContent = 'Queued ✓';
    } catch (_) {
        status.className = 'test-feedback crit';
        status.textContent = 'Failed';
    }
    setTimeout(() => {
        button.disabled = false;
        status.className = 'test-feedback';
        status.textContent = '';
    }, 2000);
}

setInterval(refreshDashboard, 5000);
document.addEventListener('visibilitychange', () => {
    if (!document.hidden) refreshDashboard();
});
