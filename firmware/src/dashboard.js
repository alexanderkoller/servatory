let dashboardBusy = false;
let refreshFailed = false;

function formatSnapshotAge(seconds) {
    if (seconds === 0) return 'Updated just now';
    if (seconds === 1) return 'Updated 1 second ago';
    return `Updated ${seconds} seconds ago`;
}

function initializeSnapshotClock(shell) {
    shell.dataset.ageStarted = Date.now();
}

function updateSnapshotAge() {
    const shell = document.querySelector('.shell');
    if (!shell) return;
    const ageLabel = shell.querySelector('.snapshot-age');
    const badge = shell.querySelector('.live');
    const baseAge = Number(shell.dataset.snapshotAge);
    const staleAfter = Number(shell.dataset.staleAfter);
    const elapsed = Math.floor((Date.now() - Number(shell.dataset.ageStarted)) / 1000);
    const hasSnapshot = baseAge >= 0;
    const age = hasSnapshot ? baseAge + Math.max(0, elapsed) : 0;
    const stale = !hasSnapshot || age >= staleAfter;
    if (ageLabel) ageLabel.textContent = hasSnapshot ? formatSnapshotAge(age) : 'No host snapshot received';
    if (badge) badge.textContent = !hasSnapshot ? 'NO DATA' : stale ? 'STALE' : refreshFailed ? 'RETRYING' : 'LIVE';
    shell.classList.toggle('is-stale', stale);
    shell.classList.toggle('refresh-failed', refreshFailed);
}

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
        refreshFailed = false;
        initializeSnapshotClock(freshShell);
        updateSnapshotAge();
    } catch (_) {
        refreshFailed = true;
        updateSnapshotAge();
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

const initialShell = document.querySelector('.shell');
if (initialShell) initializeSnapshotClock(initialShell);
updateSnapshotAge();
setInterval(updateSnapshotAge, 1000);
setInterval(refreshDashboard, 5000);
document.addEventListener('visibilitychange', () => {
    updateSnapshotAge();
    refreshDashboard();
});
