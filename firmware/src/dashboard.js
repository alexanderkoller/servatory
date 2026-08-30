let dashboardBusy = false;

async function refreshDashboard() {
    if (dashboardBusy || document.hidden) return;
    dashboardBusy = true;
    try {
        const response = await fetch('/api/v1/dashboard-fragment', { cache: 'no-store' });
        if (!response.ok) throw new Error('dashboard refresh failed');
        const template = document.createElement('template');
        template.innerHTML = await response.text();
        const freshShell = template.content.querySelector('.shell');
        const currentShell = document.querySelector('.shell');
        if (!freshShell || !currentShell) throw new Error('dashboard content missing');
        document.body.className = freshShell.dataset.bodyClass;
        currentShell.replaceWith(freshShell);
    } catch (_) {
        // Keep the last usable dashboard visible; the next interval retries.
    } finally {
        dashboardBusy = false;
    }
}

async function sendTestNotification(button) {
    if (dashboardBusy) return;
    dashboardBusy = true;
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
        dashboardBusy = false;
    }, 2000);
}

async function copyNtfyTopic(button) {
    const topic = document.getElementById('topic');
    if (!topic) return;
    try {
        await navigator.clipboard.writeText(topic.textContent.trim());
    } catch (_) {
        const range = document.createRange();
        range.selectNodeContents(topic);
        const selection = getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        document.execCommand('copy');
        selection.removeAllRanges();
    }
    const original = button.textContent;
    button.textContent = 'Copied';
    setTimeout(() => { button.textContent = original; }, 1600);
}

setInterval(refreshDashboard, 5000);
document.addEventListener('visibilitychange', refreshDashboard);
