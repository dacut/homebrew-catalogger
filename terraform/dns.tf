data "aws_route53_zone" "site" {
  name         = var.hosted_zone_name
  private_zone = false
}

# IPv4 and IPv6 alias records pointing the site's name at the distribution.
resource "aws_route53_record" "site" {
  for_each = toset(["A", "AAAA"])

  zone_id = data.aws_route53_zone.site.zone_id
  name    = var.domain_name
  type    = each.key

  alias {
    name                   = aws_cloudfront_distribution.site.domain_name
    zone_id                = aws_cloudfront_distribution.site.hosted_zone_id
    evaluate_target_health = false
  }
}
