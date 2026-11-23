variable "domain_name" {
  description = "The site's host name, e.g. \"brew.example.com\". Must be in the Route 53 zone named by hosted_zone_name."
  type        = string

  validation {
    condition     = can(regex("^([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\\.)+[a-z]{2,63}$", var.domain_name))
    error_message = "domain_name must be a lowercase DNS host name such as \"brew.example.com\"."
  }
}

variable "hosted_zone_name" {
  description = "The public Route 53 hosted zone the site's records go in, e.g. \"example.com\"."
  type        = string
}

variable "bucket_name" {
  description = "The S3 bucket for the site's files. Defaults to the domain name with dots replaced by hyphens, plus \"-site\"."
  type        = string
  default     = null
}

variable "bucket_namespace" {
  description = "Namespace for the bucket. Either \"global\" or \"account-regional\". Defaults to \"global\"."
  type        = string
  default     = "global"

  validation {
    condition     = contains(["global", "account-regional"], var.bucket_namespace)
    error_message = "bucket_namespace must be global or account-regional."
  }
}

variable "region" {
  description = "The AWS region for the S3 bucket. (The certificate is always created in us-east-1, as CloudFront requires.)"
  type        = string
  default     = "us-east-1"
}

variable "price_class" {
  description = "The CloudFront price class: PriceClass_100 (North America and Europe), PriceClass_200, or PriceClass_All."
  type        = string
  default     = "PriceClass_100"

  validation {
    condition     = contains(["PriceClass_100", "PriceClass_200", "PriceClass_All"], var.price_class)
    error_message = "price_class must be PriceClass_100, PriceClass_200, or PriceClass_All."
  }
}

variable "tags" {
  description = "Tags applied to every resource."
  type        = map(string)
  default     = {}
}

locals {
  # Used to name CloudFront resources, whose names allow letters, digits, hyphens, and underscores.
  name_prefix = substr(replace(var.domain_name, ".", "-"), 0, 40)
  bucket_name = coalesce(var.bucket_name, "${replace(var.domain_name, ".", "-")}-site")
  zone_name   = trimsuffix(data.aws_route53_zone.site.name, ".")
}

variable "enable_automation" {
  description = "Deploy a scheduled Lambda function that keeps the catalog and site up to date, classifying new formulae with Claude in Amazon Bedrock. Build it first with scripts/build-lambda.sh."
  type        = bool
  default     = false
}

variable "automation_schedule" {
  description = "When the update function runs, as an EventBridge schedule expression."
  type        = string
  default     = "rate(1 day)"
}

variable "lambda_package" {
  description = "The update function's deployment package. Defaults to the output of scripts/build-lambda.sh."
  type        = string
  default     = null
}

variable "bedrock_model" {
  description = "The Claude model the update function classifies with: a plain name such as \"claude-haiku-5-5\" (invoked through the inference profile for the region's geography) or a full inference profile ID."
  type        = string
  default     = "claude-haiku-5-5"

  validation {
    condition     = can(regex("^([a-z-]+\\.)?(anthropic\\.)?claude-[a-z0-9.-]+$", var.bedrock_model))
    error_message = "bedrock_model must be a Claude model name (e.g. \"claude-haiku-5-5\") or an inference profile ID (e.g. \"us.anthropic.claude-haiku-5-5\")."
  }
}

variable "bedrock_geography" {
  description = "The cross-Region inference geography for a plain bedrock_model name: us, eu, apac, global, .... Defaults from the region."
  type        = string
  default     = null
}

variable "classify_concurrency" {
  description = "How many Bedrock requests the update function runs at once."
  type        = number
  default     = 8

  validation {
    condition     = var.classify_concurrency >= 1
    error_message = "classify_concurrency must be at least 1."
  }
}

variable "automation_log_retention_days" {
  description = "How long the update function's logs are kept."
  type        = number
  default     = 30
}
