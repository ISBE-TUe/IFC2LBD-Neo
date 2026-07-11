import "./pipeline/app.js";

// Orientation overlay for phones
function isPortrait() {
	return window.innerHeight > window.innerWidth;
}
function isTouchDevice() {
	return "ontouchstart" in window || navigator.maxTouchPoints > 0;
}
function checkOrientation() {
	const overlay = document.getElementById("rotate-overlay");
	if (!overlay) return;
	overlay.style.display = isTouchDevice() && isPortrait() ? "flex" : "none";
}
window.addEventListener("resize", checkOrientation);
window.addEventListener("orientationchange", checkOrientation);
checkOrientation();
