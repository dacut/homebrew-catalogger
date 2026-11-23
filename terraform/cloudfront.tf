# The CloudFront distribution serving the site over HTTPS from the private bucket.

resource "aws_cloudfront_origin_access_control" "site" {
  name                              = local.name_prefix
  description                       = "Signs CloudFront requests to the ${local.bucket_name} bucket"
  origin_access_control_origin_type = "s3"
  signing_behavior                  = "always"
  signing_protocol                  = "sigv4"
}

# Redirects folder URLs (e.g. /categories/) to the folder's index.html. See the comments in the function's source.
resource "aws_cloudfront_function" "folder_index" {
  name    = "${local.name_prefix}-folder-index"
  comment = "Redirect folder URLs to their index.html"
  runtime = "cloudfront-js-2.0"
  publish = true
  code    = file("${path.module}/functions/folder-index.js")
}

# Caches according to the objects' Cache-Control headers (default 1 day), with gzip and Brotli compression.
data "aws_cloudfront_cache_policy" "caching_optimized" {
  name = "Managed-CachingOptimized"
}

resource "aws_cloudfront_response_headers_policy" "site" {
  name    = "${local.name_prefix}-security-headers"
  comment = "Security headers for ${var.domain_name}"

  security_headers_config {
    # The site runs no scripts and loads nothing from other origins. Inline styles are allowed for the 404 page,
    # which inlines its stylesheet so it renders at any URL.
    content_security_policy {
      content_security_policy = join("; ", [
        "default-src 'none'",
        "style-src 'self' 'unsafe-inline'",
        "img-src 'self'",
        "base-uri 'none'",
        "form-action 'none'",
        "frame-ancestors 'none'",
      ])
      override = true
    }

    content_type_options {
      override = true
    }

    frame_options {
      frame_option = "DENY"
      override     = true
    }

    referrer_policy {
      referrer_policy = "strict-origin-when-cross-origin"
      override        = true
    }

    strict_transport_security {
      access_control_max_age_sec = 31536000
      include_subdomains         = false
      preload                    = false
      override                   = true
    }
  }
}

resource "aws_cloudfront_distribution" "site" {
  enabled             = true
  comment             = "Homebrew formula catalog (${var.domain_name})"
  aliases             = [var.domain_name]
  default_root_object = "index.html"
  http_version        = "http2and3"
  is_ipv6_enabled     = true
  price_class         = var.price_class

  origin {
    origin_id                = "site-bucket"
    domain_name              = aws_s3_bucket.site.bucket_regional_domain_name
    origin_access_control_id = aws_cloudfront_origin_access_control.site.id
  }

  default_cache_behavior {
    target_origin_id           = "site-bucket"
    viewer_protocol_policy     = "redirect-to-https"
    allowed_methods            = ["GET", "HEAD"]
    cached_methods             = ["GET", "HEAD"]
    compress                   = true
    cache_policy_id            = data.aws_cloudfront_cache_policy.caching_optimized.id
    response_headers_policy_id = aws_cloudfront_response_headers_policy.site.id

    function_association {
      event_type   = "viewer-request"
      function_arn = aws_cloudfront_function.folder_index.arn
    }
  }

  # S3 returns 403 for a missing object (the bucket policy doesn't grant s3:ListBucket); serve the 404 page for it.
  dynamic "custom_error_response" {
    for_each = [403, 404]

    content {
      error_code            = custom_error_response.value
      response_code         = 404
      response_page_path    = "/404.html"
      error_caching_min_ttl = 60
    }
  }

  restrictions {
    geo_restriction {
      restriction_type = "none"
    }
  }

  viewer_certificate {
    acm_certificate_arn      = aws_acm_certificate_validation.site.certificate_arn
    ssl_support_method       = "sni-only"
    minimum_protocol_version = "TLSv1.2_2021"
  }
}
