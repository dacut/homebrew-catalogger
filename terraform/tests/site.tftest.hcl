# Offline tests using a mocked AWS provider: `terraform init -backend=false && terraform test`.

mock_provider "aws" {
  override_data {
    target = data.aws_route53_zone.site
    values = {
      name    = "example.com."
      zone_id = "Z0123456789EXAMPLE"
    }
  }

  mock_data "aws_iam_policy_document" {
    defaults = {
      json = "{}"
    }
  }

  mock_data "aws_caller_identity" {
    defaults = {
      account_id = "123456789012"
    }
  }

  mock_data "aws_partition" {
    defaults = {
      partition = "aws"
    }
  }

  # The provider validates ARN formats, so mocked ARNs must be well-formed.
  mock_resource "aws_cloudfront_function" {
    defaults = {
      arn = "arn:aws:cloudfront::123456789012:function/brew-example-com-folder-index"
    }
  }

  mock_resource "aws_cloudfront_distribution" {
    defaults = {
      arn            = "arn:aws:cloudfront::123456789012:distribution/EDFDVBD6EXAMPLE"
      domain_name    = "d111111abcdef8.cloudfront.net"
      hosted_zone_id = "Z2FDTNDATAQYW2"
    }
  }

  mock_resource "aws_s3_bucket" {
    defaults = {
      arn = "arn:aws:s3:::brew-example-com-site"
    }
  }

  mock_resource "aws_iam_role" {
    defaults = {
      arn = "arn:aws:iam::123456789012:role/brew-example-com-update"
    }
  }

  mock_resource "aws_lambda_function" {
    defaults = {
      arn = "arn:aws:lambda:us-east-1:123456789012:function:brew-example-com-update"
    }
  }

  mock_resource "aws_cloudwatch_event_rule" {
    defaults = {
      arn = "arn:aws:events:us-east-1:123456789012:rule/brew-example-com-update"
    }
  }
}

mock_provider "aws" {
  alias = "us_east_1"

  mock_resource "aws_acm_certificate" {
    defaults = {
      arn = "arn:aws:acm:us-east-1:123456789012:certificate/00000000-0000-0000-0000-000000000000"
      domain_validation_options = [{
        domain_name           = "brew.example.com"
        resource_record_name  = "_abc123.brew.example.com."
        resource_record_type  = "CNAME"
        resource_record_value = "_def456.acm-validations.aws."
      }]
    }
  }
}

variables {
  domain_name      = "brew.example.com"
  hosted_zone_name = "example.com"
}

run "creates_site" {
  command = apply

  assert {
    condition     = aws_s3_bucket.site.bucket == "brew-example-com-site"
    error_message = "The bucket name should default to the domain name with hyphens."
  }

  assert {
    condition     = aws_cloudfront_distribution.site.aliases == toset(["brew.example.com"])
    error_message = "The distribution should serve the site's domain."
  }

  assert {
    condition     = aws_cloudfront_distribution.site.default_root_object == "index.html"
    error_message = "The site root should serve index.html."
  }

  assert {
    condition     = one(aws_cloudfront_distribution.site.default_cache_behavior[0].function_association).event_type == "viewer-request"
    error_message = "The folder-index function should run on viewer requests."
  }

  assert {
    condition     = aws_cloudfront_function.folder_index.runtime == "cloudfront-js-2.0"
    error_message = "The folder-index function should use the cloudfront-js-2.0 runtime."
  }

  assert {
    condition     = toset(keys(aws_route53_record.site)) == toset(["A", "AAAA"])
    error_message = "Both IPv4 and IPv6 alias records should be created."
  }

  assert {
    condition     = aws_route53_record.certificate_validation.name == "_abc123.brew.example.com."
    error_message = "The certificate's DNS validation record should be created in the zone."
  }

  assert {
    condition     = output.site_url == "https://brew.example.com/"
    error_message = "Unexpected site URL."
  }
}

run "custom_bucket_name" {
  command = apply

  variables {
    bucket_name = "my-bucket"
  }

  assert {
    condition     = aws_s3_bucket.site.bucket == "my-bucket"
    error_message = "bucket_name should override the default."
  }
}

run "rejects_domain_outside_zone" {
  command = plan

  variables {
    domain_name = "brew.example.org"
  }

  expect_failures = [aws_acm_certificate.site]
}

run "rejects_invalid_domain" {
  command = plan

  variables {
    domain_name = "Brew.Example.com"
  }

  expect_failures = [var.domain_name]
}

run "automation_is_off_by_default" {
  command = plan

  assert {
    condition     = length(aws_lambda_function.update) == 0 && length(aws_cloudwatch_event_rule.update) == 0
    error_message = "No update function or schedule should be created unless enable_automation is true."
  }
}

run "automation_enabled" {
  command = apply

  variables {
    enable_automation = true
    # Any existing file stands in for the deployment package.
    lambda_package = "functions/folder-index.js"
    bedrock_model  = "us.anthropic.claude-haiku-5-5"
  }

  assert {
    condition     = aws_lambda_function.update[0].runtime == "provided.al2023" && aws_lambda_function.update[0].architectures == tolist(["arm64"])
    error_message = "The update function should run the Rust bootstrap on Arm64."
  }

  assert {
    condition     = aws_lambda_function.update[0].environment[0].variables["SITE_BUCKET"] == aws_s3_bucket.site.id
    error_message = "The update function should be told the site bucket."
  }

  assert {
    condition     = aws_lambda_function.update[0].environment[0].variables["CATALOG_KEY"] == "formula-catalogged.json"
    error_message = "The update function should read the catalog from the bucket root."
  }

  assert {
    condition     = aws_lambda_function.update[0].environment[0].variables["SITE_URL"] == "https://brew.example.com/"
    error_message = "The update function needs the site's URL for the sitemap and canonical links."
  }

  assert {
    condition     = !contains(keys(aws_lambda_function.update[0].environment[0].variables), "BEDROCK_GEOGRAPHY")
    error_message = "BEDROCK_GEOGRAPHY should only be set when bedrock_geography is."
  }

  assert {
    condition     = local.bedrock_model_name == "claude-haiku-5-5"
    error_message = "IAM permissions should be scoped to the model, whatever prefix bedrock_model has."
  }

  assert {
    condition     = aws_cloudwatch_event_rule.update[0].schedule_expression == "rate(1 day)"
    error_message = "The update should run daily by default."
  }

  assert {
    condition     = aws_lambda_function_event_invoke_config.update[0].maximum_retry_attempts == 0
    error_message = "Failed runs should wait for the next schedule rather than retrying immediately."
  }

  assert {
    condition     = aws_lambda_permission.update_schedule[0].principal == "events.amazonaws.com"
    error_message = "The schedule should be allowed to invoke the function."
  }

  assert {
    condition     = output.update_function_name == "brew-example-com-update"
    error_message = "Unexpected update function name."
  }
}

run "automation_requires_a_built_package" {
  command = plan

  variables {
    enable_automation = true
    lambda_package    = "does-not-exist.zip"
  }

  expect_failures = [aws_lambda_function.update]
}

run "rejects_invalid_bedrock_model" {
  command = plan

  variables {
    bedrock_model = "gpt-5"
  }

  expect_failures = [var.bedrock_model]
}
