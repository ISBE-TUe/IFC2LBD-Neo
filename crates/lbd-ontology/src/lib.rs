//! LBD and related namespace constants plus lightweight RDF value types.

pub const BOT: &str = "https://w3id.org/bot#";
pub const TOPO: &str = "https://w3id.org/ifc2lbd/topology#";
pub const LBD: &str = "https://linkedbuildingdata.org/LBD#";
pub const BEO: &str = "https://pi.pauwel.be/voc/buildingelement#";
pub const FURN: &str = "http://pi.pauwel.be/voc/furniture#";
pub const PROPS: &str = "http://lbd.arch.rwth-aachen.de/props#";
pub const UNIT: &str = "http://qudt.org/vocab/unit/";
pub const LIST: &str = "https://w3id.org/list#";
pub const EXPRESS: &str = "https://w3id.org/express#";
pub const OPM: &str = "https://w3id.org/opm#";
pub const BSDDC: &str = "https://identifier.buildingsmart.org/uri/buildingsmart/ifc/4.3/class/";
pub const BSDDP: &str = "https://identifier.buildingsmart.org/uri/buildingsmart/ifc/4.3/prop/";
pub const BSDDM: &str = "https://w3id.org/ifc2lbd/bsdd-meta#";
pub const OMG: &str = "https://w3id.org/omg#";
pub const GEO: &str = "http://www.opengis.net/ont/geosparql#";
pub const SCHEMA: &str = "http://schema.org/";
pub const SMLS: &str = "https://w3id.org/def/smls-owl#";
pub const PROV: &str = "http://www.w3.org/ns/prov#";
pub const OWL: &str = "http://www.w3.org/2002/07/owl#";
pub const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
pub const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

pub const PREFIXES: [(&str, &str); 22] = [
    ("bot", BOT),
    ("topo", TOPO),
    ("beo", BEO),
    ("furn", FURN),
    ("lbd", LBD),
    ("props", PROPS),
    ("unit", UNIT),
    ("list", LIST),
    ("express", EXPRESS),
    ("opm", OPM),
    ("bsddc", BSDDC),
    ("bsddp", BSDDP),
    ("bsddm", BSDDM),
    ("omg", OMG),
    ("geo", GEO),
    ("schema", SCHEMA),
    ("smls", SMLS),
    ("prov", PROV),
    ("owl", OWL),
    ("rdf", RDF),
    ("rdfs", RDFS),
    ("xsd", XSD),
];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Object {
    Iri(String),
    Literal(String),
    TypedLiteral { value: String, datatype: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Triple {
    pub subject: String,
    pub predicate: String,
    pub object: Object,
}

pub fn rdf_type() -> String {
    format!("{RDF}type")
}

pub fn rdf_member() -> String {
    format!("{RDF}member")
}

pub fn rdf_seq() -> String {
    format!("{RDF}Seq")
}

pub fn rdf_li(n: usize) -> String {
    format!("{RDF}_{}", n)
}

pub fn owl_same_as() -> String {
    format!("{OWL}sameAs")
}

pub fn owl_ontology() -> String {
    format!("{OWL}Ontology")
}

pub fn owl_imports() -> String {
    format!("{OWL}imports")
}

pub fn owl_datatype_property() -> String {
    format!("{OWL}DatatypeProperty")
}

pub fn owl_object_property() -> String {
    format!("{OWL}ObjectProperty")
}

pub fn rdfs_class() -> String {
    format!("{RDFS}Class")
}

pub fn rdfs_comment() -> String {
    format!("{RDFS}comment")
}

pub fn rdfs_label() -> String {
    format!("{RDFS}label")
}

pub fn lbd_property_set() -> String {
    format!("{LBD}PropertySet")
}

pub fn lbd_has_property_set() -> String {
    format!("{LBD}hasPropertySets")
}

pub fn lbd_quantity_set() -> String {
    format!("{LBD}ElementQuantitySet")
}

pub fn lbd_has_quantity_set() -> String {
    format!("{LBD}hasQuantitySet")
}

pub fn lbd_project() -> String {
    format!("{LBD}Project")
}

pub fn lbd_has_bounding_box() -> String {
    format!("{LBD}hasBoundingBox")
}

pub fn lbd_x_min() -> String {
    format!("{LBD}x-min")
}

pub fn lbd_x_max() -> String {
    format!("{LBD}x-max")
}

pub fn lbd_y_min() -> String {
    format!("{LBD}y-min")
}

pub fn lbd_y_max() -> String {
    format!("{LBD}y-max")
}

pub fn lbd_z_min() -> String {
    format!("{LBD}z-min")
}

pub fn lbd_z_max() -> String {
    format!("{LBD}z-max")
}

pub fn bot_zone() -> String {
    format!("{BOT}Zone")
}

pub fn bot_site() -> String {
    format!("{BOT}Site")
}

pub fn bot_building() -> String {
    format!("{BOT}Building")
}

pub fn bot_storey() -> String {
    format!("{BOT}Storey")
}

pub fn bot_space() -> String {
    format!("{BOT}Space")
}

pub fn bot_element() -> String {
    format!("{BOT}Element")
}

pub fn topo_predicate(local_name: &str) -> String {
    format!("{TOPO}{local_name}")
}

pub fn beo_class(local_name: &str) -> String {
    format!("{BEO}{local_name}")
}

pub fn furn_class(local_name: &str) -> String {
    format!("{FURN}{local_name}")
}

pub fn props_property(local_name: &str) -> String {
    format!("{PROPS}{local_name}")
}

pub fn unit_iri(local_name: &str) -> String {
    format!("{UNIT}{local_name}")
}

pub fn opm_property() -> String {
    format!("{OPM}Property")
}

pub fn omg_has_geometry() -> String {
    format!("{OMG}hasGeometry")
}

pub fn omg_geometry() -> String {
    format!("{OMG}Geometry")
}

pub fn geo_has_geometry() -> String {
    format!("{GEO}hasGeometry")
}

pub fn geo_geometry() -> String {
    format!("{GEO}Geometry")
}

pub fn geo_as_wkt() -> String {
    format!("{GEO}asWKT")
}

pub fn geo_wkt_literal() -> String {
    format!("{GEO}wktLiteral")
}

pub fn opm_current_property_state() -> String {
    format!("{OPM}CurrentPropertyState")
}

pub fn opm_has_property_state() -> String {
    format!("{OPM}hasPropertyState")
}

pub fn opm_value() -> String {
    format!("{OPM}value")
}

pub fn schema_value() -> String {
    format!("{SCHEMA}value")
}

pub fn schema_applicable_type() -> String {
    format!("{SCHEMA}applicableType")
}

pub fn smls_unit() -> String {
    format!("{SMLS}unit")
}

pub fn prov_generated_at_time() -> String {
    format!("{PROV}generatedAtTime")
}

pub fn list_has_contents() -> String {
    format!("{LIST}hasContents")
}

pub fn list_has_next() -> String {
    format!("{LIST}hasNext")
}

pub fn express_has_string() -> String {
    format!("{EXPRESS}hasString")
}

pub fn express_has_double() -> String {
    format!("{EXPRESS}hasDouble")
}

pub fn express_has_integer() -> String {
    format!("{EXPRESS}hasInteger")
}

pub fn express_has_boolean() -> String {
    format!("{EXPRESS}hasBoolean")
}

pub fn express_has_logical() -> String {
    format!("{EXPRESS}hasLogical")
}

pub fn express_logical_value(value: bool) -> String {
    format!("{EXPRESS}{}", if value { "TRUE" } else { "FALSE" })
}

pub fn bot_has_site() -> String {
    format!("{BOT}hasSite")
}

pub fn bot_has_building() -> String {
    format!("{BOT}hasBuilding")
}

pub fn bot_has_storey() -> String {
    format!("{BOT}hasStorey")
}

pub fn bot_has_space() -> String {
    format!("{BOT}hasSpace")
}

pub fn bot_contains_element() -> String {
    format!("{BOT}containsElement")
}

pub fn bot_contains_zone() -> String {
    format!("{BOT}containsZone")
}

pub fn bot_adjacent_element() -> String {
    format!("{BOT}adjacentElement")
}

pub fn bot_adjacent_zone() -> String {
    format!("{BOT}adjacentZone")
}

pub fn bot_has_sub_element() -> String {
    format!("{BOT}hasSubElement")
}

pub fn bot_intersecting_element() -> String {
    format!("{BOT}intersectingElement")
}

pub fn bot_interface() -> String {
    format!("{BOT}Interface")
}

pub fn bot_interface_of() -> String {
    format!("{BOT}interfaceOf")
}
