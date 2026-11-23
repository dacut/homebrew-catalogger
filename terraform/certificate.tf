# The TLS certificate for the site, validated through DNS records in the hosted zone.

resource "aws_acm_certificate" "site" {
  provider          = aws.us_east_1
  domain_name       = var.domain_name
  validation_method = "DNS"

  lifecycle {
    create_before_destroy = true

    precondition {
      condition     = var.domain_name == local.zone_name || endswith(var.domain_name, ".${local.zone_name}")
      error_message = "domain_name (${var.domain_name}) is not in the hosted zone ${local.zone_name}."
    }
  }
}

locals {
  # The certificate covers a single name, so ACM issues exactly one validation record.
  certificate_validation = one(aws_acm_certificate.site.domain_validation_options)
}

resource "aws_route53_record" "certificate_validation" {
  zone_id         = data.aws_route53_zone.site.zone_id
  name            = local.certificate_validation.resource_record_name
  type            = local.certificate_validation.resource_record_type
  records         = [local.certificate_validation.resource_record_value]
  ttl             = 300
  allow_overwrite = true
}

resource "aws_acm_certificate_validation" "site" {
  provider                = aws.us_east_1
  certificate_arn         = aws_acm_certificate.site.arn
  validation_record_fqdns = [aws_route53_record.certificate_validation.fqdn]
}
