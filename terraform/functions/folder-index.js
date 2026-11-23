// CloudFront viewer-request function (runtime cloudfront-js-2.0).
//
// Browsing a folder redirects to that folder's index.html:
//
//   /categories/  -> 301 /categories/index.html
//   /categories   -> 301 /categories/index.html  (a final path segment without a "." is treated as a folder)
//
// The site root is the exception: "/" serves /index.html in place, so the home page keeps its short URL.
//
// Redirect targets are built only from the request path, never from headers, and are always same-site paths, so
// this cannot be used as an open redirect. Query strings are dropped; the site doesn't use them.

function handler(event) {
    var request = event.request;
    var uri = request.uri;

    if (uri === "/") {
        request.uri = "/index.html";
        return request;
    }

    var lastSegment = uri.substring(uri.lastIndexOf("/") + 1);
    var target;
    if (uri.endsWith("/")) {
        target = uri + "index.html";
    } else if (lastSegment.indexOf(".") === -1) {
        target = uri + "/index.html";
    } else {
        return request;
    }

    // A Location starting with "//" (or "/\", which browsers treat the same way) would point at another host.
    // Collapse leading slashes and encode backslashes so the target stays on this site.
    target = "/" + target.replace(/^[\/\\]+/, "").replace(/\\/g, "%5C");

    return {
        statusCode: 301,
        statusDescription: "Moved Permanently",
        headers: {
            location: { value: target },
            "cache-control": { value: "max-age=3600" },
        },
    };
}
