# Automatic site updates (only when enable_automation is true): a scheduled Lambda function that downloads the
# current catalog and Homebrew's latest formula.json, classifies new formulae with Claude in Amazon Bedrock, uploads
# the new catalog and regenerated site, and invalidates the CloudFront cache. Build the function first with
# scripts/build-lambda.sh.

locals {
  automation  = var.enable_automation ? 1 : 0
  catalog_key = "formula-catalogged.json"

  lambda_package = coalesce(var.lambda_package, "${path.module}/../target/lambda/homebrew-catalog-lambda/bootstrap.zip")

  # The model name without any inference profile or provider prefix, e.g. "claude-haiku-5-5", for IAM resources.
  bedrock_model_name = regex("claude-[a-z0-9.-]+$", var.bedrock_model)

  update_function_name = "${local.name_prefix}-update"
}

data "aws_caller_identity" "current" {}

data "aws_partition" "current" {}

data "aws_iam_policy_document" "update_assume_role" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["lambda.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "update" {
  count = local.automation

  name               = local.update_function_name
  description        = "Runs the ${var.domain_name} site update function"
  assume_role_policy = data.aws_iam_policy_document.update_assume_role.json
}

data "aws_iam_policy_document" "update" {
  statement {
    sid       = "WriteLogs"
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents"]
    resources = ["arn:${data.aws_partition.current.partition}:logs:${var.region}:${data.aws_caller_identity.current.account_id}:log-group:/aws/lambda/${local.update_function_name}:*"]
  }

  statement {
    sid       = "ListSiteBucket"
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.site.arn]
  }

  statement {
    sid       = "UpdateSiteObjects"
    actions   = ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"]
    resources = ["${aws_s3_bucket.site.arn}/*"]
  }

  statement {
    sid       = "InvalidateSiteCache"
    actions   = ["cloudfront:CreateInvalidation"]
    resources = [aws_cloudfront_distribution.site.arn]
  }

  # Cross-Region inference: the inference profile (any geography) in this account, and the foundation model in
  # whichever Region the profile routes the request to (an empty Region for global profiles).
  statement {
    sid     = "ClassifyWithBedrock"
    actions = ["bedrock:InvokeModel"]
    resources = [
      "arn:${data.aws_partition.current.partition}:bedrock:*:${data.aws_caller_identity.current.account_id}:inference-profile/*.anthropic.${local.bedrock_model_name}",
      "arn:${data.aws_partition.current.partition}:bedrock:*::foundation-model/anthropic.${local.bedrock_model_name}",
    ]
  }
}

resource "aws_iam_role_policy" "update" {
  count = local.automation

  name   = "update-site"
  role   = aws_iam_role.update[0].id
  policy = data.aws_iam_policy_document.update.json
}

resource "aws_cloudwatch_log_group" "update" {
  count = local.automation

  name              = "/aws/lambda/${local.update_function_name}"
  retention_in_days = var.automation_log_retention_days
}

resource "aws_lambda_function" "update" {
  count = local.automation

  function_name = local.update_function_name
  description   = "Updates the ${var.domain_name} Homebrew formula catalog and website"
  role          = aws_iam_role.update[0].arn

  runtime          = "provided.al2023"
  handler          = "bootstrap"
  architectures    = ["arm64"]
  filename         = local.lambda_package
  source_code_hash = fileexists(local.lambda_package) ? filebase64sha256(local.lambda_package) : null

  # Parsing two copies of formula.json (about 32 MB each) needs room; more memory also means more CPU.
  memory_size = 2048
  timeout     = 900

  ephemeral_storage {
    size = 1024
  }

  environment {
    variables = merge(
      {
        SITE_BUCKET          = aws_s3_bucket.site.id
        DISTRIBUTION_ID      = aws_cloudfront_distribution.site.id
        CATALOG_KEY          = local.catalog_key
        SITE_URL             = "https://${var.domain_name}/"
        BEDROCK_MODEL        = var.bedrock_model
        CLASSIFY_CONCURRENCY = tostring(var.classify_concurrency)
      },
      var.bedrock_geography == null ? {} : { BEDROCK_GEOGRAPHY = var.bedrock_geography },
    )
  }

  logging_config {
    log_format = "Text"
    log_group  = aws_cloudwatch_log_group.update[0].name
  }

  depends_on = [aws_iam_role_policy.update]

  lifecycle {
    precondition {
      condition     = fileexists(local.lambda_package)
      error_message = "The Lambda package ${local.lambda_package} doesn't exist; run scripts/build-lambda.sh first (or set lambda_package)."
    }
  }
}

# Each run picks up where the last left off, so a failed run is simply retried on the next schedule rather than
# immediately (which would repeat its work).
resource "aws_lambda_function_event_invoke_config" "update" {
  count = local.automation

  function_name          = aws_lambda_function.update[0].function_name
  maximum_retry_attempts = 0
}

resource "aws_cloudwatch_event_rule" "update" {
  count = local.automation

  name                = local.update_function_name
  description         = "Updates the ${var.domain_name} site"
  schedule_expression = var.automation_schedule
}

resource "aws_cloudwatch_event_target" "update" {
  count = local.automation

  rule = aws_cloudwatch_event_rule.update[0].name
  arn  = aws_lambda_function.update[0].arn
}

resource "aws_lambda_permission" "update_schedule" {
  count = local.automation

  statement_id  = "AllowScheduledInvocation"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.update[0].function_name
  principal     = "events.amazonaws.com"
  source_arn    = aws_cloudwatch_event_rule.update[0].arn
}
