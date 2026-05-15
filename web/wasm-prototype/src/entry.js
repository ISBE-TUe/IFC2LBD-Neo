if (window.innerWidth < 900) {
  document.getElementById("pipeline-view").style.display = "none";
  document.getElementById("mobile-view").style.display = "block";
  import("./main.js");
} else {
  document.getElementById("mobile-view").remove();
  import("./pipeline/app.js");
}
